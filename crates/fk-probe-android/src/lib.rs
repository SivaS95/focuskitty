//! Android implementation of [`ActivityProbe`].
//!
//! Talks to the platform through JNI rather than a Kotlin plugin: everything
//! then lives in this crate, with no generated Android sources to keep in step.
//!
//! # What Android will and will not tell you
//!
//! `UsageStatsManager` reports which app is in the foreground, but only after
//! the user grants **Usage Access** — a toggle on its own Settings screen, not
//! a permission dialog you can raise. Until then every query returns nothing,
//! which is indistinguishable from an idle phone, so the permission state has
//! to be checked explicitly rather than inferred from empty results.

#![cfg(target_os = "android")]

use anyhow::{anyhow, Result};
use fk_core::activity::{Activity, ActivityProbe, AppInfo};
use jni::objects::{JObject, JString, JValue};
use jni::JNIEnv;

/// The JavaVM, captured when Android loads this library.
///
/// `JNI_GetCreatedJavaVMs` is not exported to apps -- linking against it makes
/// the whole `.so` fail to load -- and `ndk-context` is never initialised by
/// Tauri. `JNI_OnLoad` is the one hook the runtime guarantees to call, and it
/// hands over the VM directly.
static JAVA_VM: std::sync::OnceLock<jni::JavaVM> = std::sync::OnceLock::new();

#[no_mangle]
pub extern "system" fn JNI_OnLoad(
    vm: *mut jni::sys::JavaVM,
    _reserved: *mut std::ffi::c_void,
) -> jni::sys::jint {
    if let Ok(vm) = unsafe { jni::JavaVM::from_raw(vm) } {
        let _ = JAVA_VM.set(vm);
    }
    jni::sys::JNI_VERSION_1_6
}

fn java_vm() -> Result<&'static jni::JavaVM> {
    JAVA_VM.get().ok_or_else(|| anyhow!("JNI_OnLoad has not run"))
}

/// The application Context, without holding a reference to an Activity.
///
/// `ActivityThread.currentApplication()` is reachable from any thread and
/// returns a Context good enough for getSystemService, the PackageManager and
/// starting an Activity (given FLAG_ACTIVITY_NEW_TASK).
fn app_context<'a>(env: &mut JNIEnv<'a>) -> Result<JObject<'a>> {
    let class = env.find_class("android/app/ActivityThread")?;
    let app = env
        .call_static_method(
            class,
            "currentApplication",
            "()Landroid/app/Application;",
            &[],
        )?
        .l()?;
    if app.is_null() {
        return Err(anyhow!("no Application yet"));
    }
    Ok(app)
}

/// Run a closure with a JNI env and the app's Context.
fn with_env<T>(f: impl FnOnce(&mut JNIEnv, &JObject) -> Result<T>) -> Result<T> {
    let vm = java_vm()?;
    let mut env = vm.attach_current_thread()?;
    let context = app_context(&mut env)?;
    let out = f(&mut env, &context);
    // A pending Java exception poisons every later call, so clear it here.
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_describe();
        let _ = env.exception_clear();
    }
    out
}

fn jstr(env: &mut JNIEnv, s: &JString) -> Result<String> {
    Ok(env.get_string(s)?.into())
}

/// Has the user granted Usage Access?
///
/// Checked directly, because without it every usage query returns an empty
/// list — identical to a phone nobody is touching.
pub fn has_usage_access() -> bool {
    with_env(|env, ctx| {
        let appops = env
            .call_method(
                ctx,
                "getSystemService",
                "(Ljava/lang/String;)Ljava/lang/Object;",
                &[(&env.new_string("appops")?).into()],
            )?
            .l()?;

        let pkg = env
            .call_method(ctx, "getPackageName", "()Ljava/lang/String;", &[])?
            .l()?;

        let uid = {
            let info = env
                .call_method(ctx, "getApplicationInfo", "()Landroid/content/pm/ApplicationInfo;", &[])?
                .l()?;
            env.get_field(&info, "uid", "I")?.i()?
        };

        let op = env.new_string("android:get_usage_stats")?;
        // unsafeCheckOpNoThrow exists from API 29; fall back for older.
        let mode = env
            .call_method(
                &appops,
                "unsafeCheckOpNoThrow",
                "(Ljava/lang/String;ILjava/lang/String;)I",
                &[(&op).into(), JValue::Int(uid), (&pkg).into()],
            )
            .or_else(|_| {
                env.call_method(
                    &appops,
                    "checkOpNoThrow",
                    "(Ljava/lang/String;ILjava/lang/String;)I",
                    &[(&op).into(), JValue::Int(uid), (&pkg).into()],
                )
            })?
            .i()?;

        Ok(mode == 0) // MODE_ALLOWED
    })
    .unwrap_or(false)
}

/// Send the user to the Usage Access settings screen.
pub fn open_usage_settings() -> Result<()> {
    with_env(|env, ctx| {
        let action = env.new_string("android.settings.USAGE_ACCESS_SETTINGS")?;
        let intent = env.new_object(
            "android/content/Intent",
            "(Ljava/lang/String;)V",
            &[(&action).into()],
        )?;
        // Starting an Activity from outside one needs its own task.
        env.call_method(
            &intent,
            "addFlags",
            "(I)Landroid/content/Intent;",
            &[JValue::Int(0x1000_0000)], // FLAG_ACTIVITY_NEW_TASK
        )?;
        env.call_method(
            ctx,
            "startActivity",
            "(Landroid/content/Intent;)V",
            &[(&intent).into()],
        )?;
        Ok(())
    })
}

/// Which package is the home screen.
///
/// Resolved once: it is a user choice that does not change while the app is
/// running, and asking the package manager every second is wasteful.
pub fn home_package() -> Option<String> {
    static HOME: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    HOME.get_or_init(|| {
        with_env(|env, ctx| {
            let action = env.new_string("android.intent.action.MAIN")?;
            let intent = env.new_object(
                "android/content/Intent",
                "(Ljava/lang/String;)V",
                &[(&action).into()],
            )?;
            let home = env.new_string("android.intent.category.HOME")?;
            env.call_method(
                &intent,
                "addCategory",
                "(Ljava/lang/String;)Landroid/content/Intent;",
                &[(&home).into()],
            )?;
            let pm = env
                .call_method(ctx, "getPackageManager", "()Landroid/content/pm/PackageManager;", &[])?
                .l()?;
            // MATCH_DEFAULT_ONLY = 0x10000
            let info = env
                .call_method(
                    &pm,
                    "resolveActivity",
                    "(Landroid/content/Intent;I)Landroid/content/pm/ResolveInfo;",
                    &[(&intent).into(), JValue::Int(0x1_0000)],
                )?
                .l()?;
            if info.is_null() {
                return Err(anyhow!("no home activity"));
            }
            let ai = env
                .get_field(&info, "activityInfo", "Landroid/content/pm/ActivityInfo;")?
                .l()?;
            let pkg = env.get_field(&ai, "packageName", "Ljava/lang/String;")?.l()?;
            jstr(env, &JString::from(pkg))
        })
        .ok()
    })
    .clone()
}

/// Put down whatever is in front by going to the home screen.
///
/// A phone has no tab to close, so this is the mobile form of the same act:
/// the app is left running and nothing is lost, but it is no longer in front
/// of you. It is also the only dismissal an ordinary app is allowed --
/// force-stopping another package needs a system signature.
pub fn go_home() -> Result<()> {
    with_env(|env, ctx| {
        let action = env.new_string("android.intent.action.MAIN")?;
        let intent = env.new_object(
            "android/content/Intent",
            "(Ljava/lang/String;)V",
            &[(&action).into()],
        )?;
        let home = env.new_string("android.intent.category.HOME")?;
        env.call_method(
            &intent,
            "addCategory",
            "(Ljava/lang/String;)Landroid/content/Intent;",
            &[(&home).into()],
        )?;
        env.call_method(
            &intent,
            "addFlags",
            "(I)Landroid/content/Intent;",
            // NEW_TASK | CLEAR_TOP: started from a Service, and the launcher
            // must come forward rather than stack another copy of itself.
            &[JValue::Int(0x1000_0000 | 0x0400_0000)],
        )?;
        env.call_method(
            ctx,
            "startActivity",
            "(Landroid/content/Intent;)V",
            &[(&intent).into()],
        )?;
        Ok(())
    })
}

/// The app's private files directory.
///
/// Android permits writing nowhere else, and the overlay Service drops its
/// requests here, so the Rust store has to be pointed at the same place.
pub fn files_dir() -> Option<String> {
    with_env(|env, ctx| {
        let f = env
            .call_method(ctx, "getFilesDir", "()Ljava/io/File;", &[])?
            .l()?;
        let p = env
            .call_method(&f, "getAbsolutePath", "()Ljava/lang/String;", &[])?
            .l()?;
        jstr(env, &JString::from(p))
    })
    .ok()
}

/// Is "Display over other apps" granted? The cat cannot float without it.
pub fn can_draw_overlay() -> bool {
    with_env(|env, ctx| {
        let cls = env.find_class("android/provider/Settings")?;
        Ok(env
            .call_static_method(
                cls,
                "canDrawOverlays",
                "(Landroid/content/Context;)Z",
                &[ctx.into()],
            )?
            .z()?)
    })
    .unwrap_or(false)
}

/// Send the user to the "Display over other apps" screen for this app.
pub fn request_overlay() -> Result<()> {
    with_env(|env, ctx| {
        let action = env.new_string("android.settings.action.MANAGE_OVERLAY_PERMISSION")?;
        let pkg = env
            .call_method(ctx, "getPackageName", "()Ljava/lang/String;", &[])?
            .l()?;
        let scheme = env.new_string("package")?;
        let uri = env.call_static_method(
            "android/net/Uri",
            "fromParts",
            "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)Landroid/net/Uri;",
            &[(&scheme).into(), (&pkg).into(), (&JObject::null()).into()],
        )?.l()?;
        let intent = env.new_object(
            "android/content/Intent",
            "(Ljava/lang/String;Landroid/net/Uri;)V",
            &[(&action).into(), (&uri).into()],
        )?;
        env.call_method(&intent, "addFlags", "(I)Landroid/content/Intent;",
            &[JValue::Int(0x1000_0000)])?;
        env.call_method(ctx, "startActivity", "(Landroid/content/Intent;)V",
            &[(&intent).into()])?;
        Ok(())
    })
}

/// Is the floating cat actually on screen?
///
/// Asked of the Service itself, which runs in this same process, so the answer
/// dies exactly when the overlay window does. A marker file could not do this:
/// an update or a kill takes the window down without `onDestroy` ever running,
/// leaving the file behind claiming a cat that is not there -- and then "hide"
/// had nothing to stop and appeared to do nothing.
pub fn overlay_running() -> bool {
    with_env(|env, ctx| {
        // Through the app's OWN class loader. `find_class` on a thread Rust
        // attached uses the system loader, which can see android.* and
        // nothing of this app -- so asking it for our Service always fails.
        let loader = env
            .call_method(ctx, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])?
            .l()?;
        let name = env.new_string("com.siva.focuskitty.CatOverlayService")?;
        let cls = env
            .call_method(
                &loader,
                "loadClass",
                "(Ljava/lang/String;)Ljava/lang/Class;",
                &[(&name).into()],
            )?
            .l()?;
        let cls = jni::objects::JClass::from(cls);
        Ok(env.call_static_method(cls, "isRunning", "()Z", &[])?.z()?)
    })
    .unwrap_or(false)
}

/// Start or stop the floating cat.
pub fn set_overlay_running(on: bool) -> Result<()> {
    with_env(|env, ctx| {
        let cls = env.new_string("com.siva.focuskitty.CatOverlayService")?;
        let name = env.new_object(
            "android/content/ComponentName",
            "(Landroid/content/Context;Ljava/lang/String;)V",
            &[ctx.into(), (&cls).into()],
        )?;
        let intent = env.new_object("android/content/Intent", "()V", &[])?;
        env.call_method(
            &intent,
            "setComponent",
            "(Landroid/content/ComponentName;)Landroid/content/Intent;",
            &[(&name).into()],
        )?;
        if on {
            // A foreground service is required for a window that outlives the
            // activity; that is also why there is a permanent notification.
            env.call_method(
                ctx,
                "startForegroundService",
                "(Landroid/content/Intent;)Landroid/content/ComponentName;",
                &[(&intent).into()],
            )?;
        } else {
            env.call_method(ctx, "stopService", "(Landroid/content/Intent;)Z",
                &[(&intent).into()])?;
        }
        Ok(())
    })
}

pub struct AndroidProbe;

impl Default for AndroidProbe {
    fn default() -> Self {
        Self
    }
}

impl AndroidProbe {
    pub fn new() -> Self {
        Self
    }
}

impl ActivityProbe for AndroidProbe {
    /// What is in front RIGHT NOW.
    ///
    /// Read from the usage *event* stream, not from `queryUsageStats`. The
    /// stats are buckets: "most recently used" is the last app that had a
    /// bucket, which is not the same question. When the app actually in front
    /// is one this skips -- FocusKitty itself, or the home screen -- the stats
    /// happily name whatever was in front before it, and the tracker then
    /// charges time to an app nobody is looking at. With enforcement on that
    /// stops being a bookkeeping error: an expired app, still winning "most
    /// recently used" from an hour ago, got the user thrown out of every
    /// screen they opened, FocusKitty's own included.
    ///
    /// A foreground transition is an event with a timestamp, so the last one
    /// before now IS what is in front, whatever it happens to be.
    fn current(&self) -> Option<Activity> {
        // Resolved before the env is borrowed: nesting one `with_env` inside
        // another's closure attaches the thread twice, and the inner guard
        // dropping takes the outer env with it.
        let home = home_package();
        with_env(|env, ctx| {
            let svc = env.new_string("usagestats")?;
            let usm = env
                .call_method(
                    ctx,
                    "getSystemService",
                    "(Ljava/lang/String;)Ljava/lang/Object;",
                    &[(&svc).into()],
                )?
                .l()?;
            if usm.is_null() {
                return Err(anyhow!("no UsageStatsManager"));
            }

            let own = env
                .call_method(ctx, "getPackageName", "()Ljava/lang/String;", &[])?
                .l()?;
            let own: String = jstr(env, &JString::from(own))?;

            // Six hours back: long enough that a phone left alone still knows
            // what is on screen, short enough to stay cheap.
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            let events = env
                .call_method(
                    &usm,
                    "queryEvents",
                    "(JJ)Landroid/app/usage/UsageEvents;",
                    &[JValue::Long(now - 6 * 60 * 60 * 1000), JValue::Long(now)],
                )?
                .l()?;
            if events.is_null() {
                return Err(anyhow!("no usage events"));
            }

            let ev = env.new_object("android/app/usage/UsageEvents$Event", "()V", &[])?;
            let mut front: Option<String> = None;
            loop {
                let more = env
                    .call_method(&events, "hasNextEvent", "()Z", &[])?
                    .z()?;
                if !more {
                    break;
                }
                env.call_method(
                    &events,
                    "getNextEvent",
                    "(Landroid/app/usage/UsageEvents$Event;)Z",
                    &[(&ev).into()],
                )?;
                // ACTIVITY_RESUMED (1) is MOVE_TO_FOREGROUND under its older
                // name; the last one wins because the stream is in time order.
                if env.call_method(&ev, "getEventType", "()I", &[])?.i()? != 1 {
                    continue;
                }
                let pkg = env
                    .call_method(&ev, "getPackageName", "()Ljava/lang/String;", &[])?
                    .l()?;
                front = Some(jstr(env, &JString::from(pkg))?);
            }

            let pkg = front.ok_or_else(|| anyhow!("no foreground event yet"))?;
            // Our own window, the system chrome, and the home screen are all
            // "nothing to watch" -- and the home screen especially, since it
            // is where the cat SENDS you when a limit runs out.
            if pkg == own || pkg == "com.android.systemui" || Some(&pkg) == home.as_ref() {
                return Err(anyhow!("nothing watchable in front"));
            }

            let name = label_for(env, ctx, &pkg).unwrap_or_else(|| pkg.clone());
            Ok(Activity { app_id: pkg, app_name: name, tab: None })
        })
        .ok()
    }

    fn installed_apps(&self) -> Vec<AppInfo> {
        with_env(|env, ctx| {
            let pm = env
                .call_method(ctx, "getPackageManager", "()Landroid/content/pm/PackageManager;", &[])?
                .l()?;

            // Only apps with a launcher entry: the user has no interest in
            // limiting a background service they have never seen.
            let action = env.new_string("android.intent.action.MAIN")?;
            let intent = env.new_object(
                "android/content/Intent",
                "(Ljava/lang/String;)V",
                &[(&action).into()],
            )?;
            let cat = env.new_string("android.intent.category.LAUNCHER")?;
            env.call_method(
                &intent,
                "addCategory",
                "(Ljava/lang/String;)Landroid/content/Intent;",
                &[(&cat).into()],
            )?;

            let list = env
                .call_method(
                    &pm,
                    "queryIntentActivities",
                    "(Landroid/content/Intent;I)Ljava/util/List;",
                    &[(&intent).into(), JValue::Int(0)],
                )?
                .l()?;

            let size = env.call_method(&list, "size", "()I", &[])?.i()?;
            let mut out = Vec::new();
            for i in 0..size {
                let ri = env
                    .call_method(&list, "get", "(I)Ljava/lang/Object;", &[JValue::Int(i)])?
                    .l()?;
                let ai = env
                    .get_field(&ri, "activityInfo", "Landroid/content/pm/ActivityInfo;")?
                    .l()?;
                let pkg_obj = env.get_field(&ai, "packageName", "Ljava/lang/String;")?.l()?;
                let id = jstr(env, &JString::from(pkg_obj))?;

                let label_obj = env
                    .call_method(
                        &ri,
                        "loadLabel",
                        "(Landroid/content/pm/PackageManager;)Ljava/lang/CharSequence;",
                        &[(&pm).into()],
                    )?
                    .l()?;
                let label_str = env
                    .call_method(&label_obj, "toString", "()Ljava/lang/String;", &[])?
                    .l()?;
                let name = jstr(env, &JString::from(label_str))?;

                out.push(AppInfo { id, name, icon_b64: None });
            }
            out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
            out.dedup_by(|a, b| a.id == b.id);
            Ok(out)
        })
        .unwrap_or_default()
    }
}

/// The human-readable label for a package.
fn label_for(env: &mut JNIEnv, ctx: &JObject, pkg: &str) -> Option<String> {
    (|| -> Result<String> {
        let pm = env
            .call_method(ctx, "getPackageManager", "()Landroid/content/pm/PackageManager;", &[])?
            .l()?;
        let jpkg = env.new_string(pkg)?;
        let info = env
            .call_method(
                &pm,
                "getApplicationInfo",
                "(Ljava/lang/String;I)Landroid/content/pm/ApplicationInfo;",
                &[(&jpkg).into(), JValue::Int(0)],
            )?
            .l()?;
        let label = env
            .call_method(
                &pm,
                "getApplicationLabel",
                "(Landroid/content/pm/ApplicationInfo;)Ljava/lang/CharSequence;",
                &[(&info).into()],
            )?
            .l()?;
        let s = env
            .call_method(&label, "toString", "()Ljava/lang/String;", &[])?
            .l()?;
        jstr(env, &JString::from(s))
    })()
    .ok()
}

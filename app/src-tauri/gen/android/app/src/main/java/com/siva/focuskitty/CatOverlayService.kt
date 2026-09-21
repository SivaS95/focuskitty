package com.siva.focuskitty

import android.app.*
import android.content.Context
import android.content.Intent
import android.graphics.Color
import android.graphics.PixelFormat
import android.os.Build
import android.os.IBinder
import android.util.TypedValue
import android.view.Gravity
import android.view.MotionEvent
import android.view.WindowManager
import android.webkit.WebView
import android.webkit.WebViewClient
import kotlin.math.abs

/**
 * The cat, floating over whatever you are doing.
 *
 * Android only lets a window sit above other apps from a Service holding a
 * TYPE_APPLICATION_OVERLAY window, and only while that Service is in the
 * foreground -- which is why there is a permanent notification. There is no
 * way around either of those.
 *
 * The window is deliberately tiny. A desktop pet can afford to be big; on a
 * phone anything that covers content is a nuisance, so the cat sits at about
 * 104dp and only grows when you tap it.
 */
class CatOverlayService : Service() {

    private lateinit var wm: WindowManager
    private var web: WebView? = null
    private lateinit var params: WindowManager.LayoutParams

    private var catPx = 0
    private var panelW = 0
    private var panelH = 0
    private var bubbleW = 0
    private var bubbleH = 0
    private var expanded = false
    private var bubbling = false
    /// Where the collapsed cat sits, so speaking can be undone exactly.
    private var catX = 0
    private var catY = 0

    companion object {
        const val CHANNEL = "focuskitty.cat"
        const val NOTE_ID = 42
        const val ACTION_STOP = "com.siva.focuskitty.STOP_CAT"

        /**
         * Whether the cat is on screen, for the rest of the app to ask.
         *
         * A field rather than a file: this Service shares the app's process,
         * so if the process goes -- an update, a force-stop, the system
         * reclaiming memory -- the overlay window goes with it and so does
         * this. A file outlives both and starts lying.
         */
        @JvmStatic
        @Volatile
        var isRunning: Boolean = false
            private set
    }

    private fun dp(v: Int) = TypedValue.applyDimension(
        TypedValue.COMPLEX_UNIT_DIP, v.toFloat(), resources.displayMetrics
    ).toInt()

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_STOP) {
            stopSelf()
            return START_NOT_STICKY
        }
        return START_STICKY
    }

    override fun onCreate() {
        super.onCreate()
        startForeground(NOTE_ID, buildNotification())
        isRunning = true

        catPx = dp(104)
        panelW = dp(312)
        // Tall enough for the live countdown, which appears above the
        // controls once the app in front is being watched.
        panelH = dp(278)
        // Just enough for a line above the cat. The window only takes this
        // much while there is something to say, then shrinks back -- a cat
        // that is permanently bubble-sized is a cat that is in the way.
        bubbleW = dp(252)
        bubbleH = dp(158)

        wm = getSystemService(Context.WINDOW_SERVICE) as WindowManager

        val wv = WebView(this)
        wv.setBackgroundColor(Color.TRANSPARENT)
        wv.settings.javaScriptEnabled = true
        wv.settings.domStorageEnabled = true
        wv.settings.allowFileAccess = true
        wv.webViewClient = WebViewClient()
        // Forward console output, or a failure inside the overlay is silent.
        wv.webChromeClient = object : android.webkit.WebChromeClient() {
            override fun onConsoleMessage(m: android.webkit.ConsoleMessage): Boolean {
                android.util.Log.i("CatOverlay", "${m.message()} @${m.lineNumber()}")
                return true
            }
        }
        wv.addJavascriptInterface(Bridge(), "FK")
        wv.loadUrl("file:///android_asset/cat/mobile.html")
        web = wv

        val type =
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O)
                WindowManager.LayoutParams.TYPE_APPLICATION_OVERLAY
            else
                @Suppress("DEPRECATION") WindowManager.LayoutParams.TYPE_PHONE

        params = WindowManager.LayoutParams(
            catPx, catPx, type,
            // NOT_FOCUSABLE keeps the keyboard and back button with the app
            // underneath; the overlay still receives touches.
            WindowManager.LayoutParams.FLAG_NOT_FOCUSABLE or
                WindowManager.LayoutParams.FLAG_LAYOUT_NO_LIMITS,
            PixelFormat.TRANSLUCENT
        ).apply {
            gravity = Gravity.TOP or Gravity.START
            x = resources.displayMetrics.widthPixels - catPx - dp(12)
            y = resources.displayMetrics.heightPixels / 2
        }

        attachDrag(wv)
        wm.addView(wv, params)
    }

    /** Drag to move; a tap that barely moves opens the tools. */
    private fun attachDrag(wv: WebView) {
        // What counts as a tap is a platform decision, not ours: the same
        // threshold every other Android view uses, so the cat feels like the
        // rest of the system rather than being fussy in its own way.
        val slop = android.view.ViewConfiguration.get(this).scaledTouchSlop
        var downX = 0f; var downY = 0f
        var startX = 0; var startY = 0
        var moved = 0f
        var draggable = true

        wv.setOnTouchListener { _, e ->
            when (e.action) {
                MotionEvent.ACTION_DOWN -> {
                    // Picking the cat up ends whatever it was saying, so the
                    // window is never sized by a bubble and a drag at once.
                    if (bubbling) {
                        wv.evaluateJavascript("window.fkBubbleOff && window.fkBubbleOff()", null)
                        setBubble(false)
                    }
                    downX = e.rawX; downY = e.rawY
                    startX = params.x; startY = params.y
                    moved = 0f
                    // Dragging is for the cat; the tools stay put while you use them.
                    draggable = !expanded || e.x > (params.width - catPx)
                    false
                }
                MotionEvent.ACTION_MOVE -> {
                    if (!draggable) return@setOnTouchListener false
                    val dx = e.rawX - downX
                    val dy = e.rawY - downY
                    moved = maxOf(moved, kotlin.math.hypot(dx, dy))
                    if (moved > slop) {
                        params.x = startX + dx.toInt()
                        params.y = startY + dy.toInt()
                        wm.updateViewLayout(wv, params)
                    }
                    moved > slop
                }
                MotionEvent.ACTION_UP -> {
                    // Only the CAT toggles the panel. Once the tools are open
                    // they occupy the rest of the window, and a tap on one of
                    // their buttons must reach the page -- not be swallowed as
                    // a tap on the animal and collapse everything.
                    val onCat = !expanded || e.x > (params.width - catPx)
                    if (moved <= slop) {
                        if (onCat) toggle() else return@setOnTouchListener false
                    } else snapToEdge(wv)
                    moved > slop
                }
                else -> false
            }
        }
    }

    /** Park against the nearest side, so it never floats mid-screen. */
    private fun snapToEdge(wv: WebView) {
        val screenW = resources.displayMetrics.widthPixels
        val w = if (expanded) panelW else if (bubbling) bubbleW else catPx
        params.x = if (params.x + w / 2 < screenW / 2) dp(12) else screenW - w - dp(12)
        wm.updateViewLayout(wv, params)
    }

    private var strollAnim: android.animation.ValueAnimator? = null

    /**
     * Carry the cat a short way along its edge.
     *
     * The rig walks on the spot -- it has a `driven` flag for exactly this --
     * and the window does the travelling, which is the only way a cat in an
     * overlay can cross the screen at all. Clamped to the screen with the
     * same margin snapToEdge uses, and `catX` is kept in step so speaking
     * still returns the cat to where it actually is.
     */
    private fun stroll(dx: Int, ms: Long) {
        val wv = web ?: return
        if (expanded || bubbling) return
        val screenW = resources.displayMetrics.widthPixels
        val from = params.x
        val to = (from + dx).coerceIn(dp(12), maxOf(dp(12), screenW - catPx - dp(12)))
        if (to == from) return
        strollAnim?.cancel()
        strollAnim = android.animation.ValueAnimator.ofInt(from, to).apply {
            duration = ms
            addUpdateListener { v ->
                if (expanded || bubbling) { cancel(); return@addUpdateListener }
                params.x = v.animatedValue as Int
                catX = params.x
                runCatching { wm.updateViewLayout(wv, params) }
            }
            start()
        }
    }

    /**
     * Make room above the cat for one line, without moving the cat.
     *
     * The cat is drawn at the window's bottom-right, so the window grows up
     * and to the left and the corner it sits in stays exactly where it was.
     * Anything else and the cat appears to jump every time it speaks.
     */
    private fun setBubble(on: Boolean) {
        val wv = web ?: return
        if (expanded || bubbling == on) return
        bubbling = on
        if (on) {
            // Remember where the cat was, rather than working backwards from
            // the grown window: the edge clamps below are not reversible, so
            // arithmetic alone walked the cat across the screen a little
            // further every time it spoke.
            catX = params.x
            catY = params.y
            params.width = bubbleW
            params.height = bubbleH
            params.x = maxOf(0, catX - (bubbleW - catPx))
            params.y = maxOf(0, catY - (bubbleH - catPx))
        } else {
            params.width = catPx
            params.height = catPx
            params.x = catX
            params.y = catY
        }
        wm.updateViewLayout(wv, params)
    }

    private fun toggle() {
        val wv = web ?: return
        // The tools have room to say things themselves; drop the bubble first
        // so the window is not sized by two things at once.
        if (bubbling) { setBubble(false); web?.evaluateJavascript("window.fkBubbleOff && window.fkBubbleOff()", null) }
        expanded = !expanded
        params.width = if (expanded) panelW else catPx
        params.height = if (expanded) panelH else catPx
        // Growing to the left when parked on the right, so it stays on screen.
        val screenW = resources.displayMetrics.widthPixels
        if (expanded && params.x + panelW > screenW) params.x = screenW - panelW - dp(12)
        // ...and up, so a cat parked low does not open its tools off the bottom.
        val screenH = resources.displayMetrics.heightPixels
        if (expanded && params.y + panelH > screenH) params.y = screenH - panelH - dp(12)
        wm.updateViewLayout(wv, params)
        wv.evaluateJavascript("window.fkExpanded && window.fkExpanded($expanded)", null)
    }

    inner class Bridge {
        /**
         * What app is in front right now.
         *
         * Read here in Kotlin rather than round-tripping to Rust: the overlay
         * runs in its own WebView with no Tauri bridge, and the whole point is
         * to choose what to watch without opening the app.
         */
        @android.webkit.JavascriptInterface
        fun currentApp(): String {
            return try {
                val usm = getSystemService(Context.USAGE_STATS_SERVICE)
                        as android.app.usage.UsageStatsManager
                val now = System.currentTimeMillis()

                // The usage *events*, not the usage stats. Stats are buckets,
                // so "most recently used" names whatever last had a bucket --
                // which after the cat throws you out of an app is still that
                // app, hours later. A foreground event has a timestamp: the
                // last one before now is what is in front, full stop.
                val events = usm.queryEvents(now - 6L * 60 * 60 * 1000, now)
                val e = android.app.usage.UsageEvents.Event()
                var front = ""
                while (events.hasNextEvent()) {
                    events.getNextEvent(e)
                    // ACTIVITY_RESUMED, known as MOVE_TO_FOREGROUND before 29.
                    if (e.eventType == 1) front = e.packageName
                }

                val skip = setOf(packageName, "com.android.systemui", homePackage())
                if (front.isEmpty() || front in skip) {
                    android.util.Log.i("CatOverlay", "currentApp: nothing watchable ($front)")
                    return ""
                }
                val pm = packageManager
                val label = runCatching {
                    pm.getApplicationLabel(pm.getApplicationInfo(front, 0)).toString()
                }.getOrDefault(front)
                android.util.Log.i("CatOverlay", "currentApp -> $front ($label)")
                "$front\u001f$label"
            } catch (e: Exception) {
                android.util.Log.i("CatOverlay", "currentApp failed: $e")
                ""
            }
        }

        /** The home screen is not something anyone wants to put a limit on. */
        private fun homePackage(): String = runCatching {
            val i = Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_HOME)
            packageManager.resolveActivity(i, android.content.pm.PackageManager.MATCH_DEFAULT_ONLY)
                ?.activityInfo?.packageName ?: ""
        }.getOrDefault("")

        /** The tracker's latest snapshot, or "" if it has not written one yet. */
        @android.webkit.JavascriptInterface
        fun status(): String = runCatching {
            java.io.File(java.io.File(filesDir, "FocusKitty"), "status.json").readText()
        }.getOrDefault("")

        /**
         * Queue a rule for the tracker to pick up.
         *
         * Written as a small request file rather than edited into the config
         * directly: the config's shape belongs to the Rust core, and two
         * writers would eventually disagree about it.
         */
        @android.webkit.JavascriptInterface
        fun watchApp(pkg: String, label: String, minutes: Int) {
            runCatching {
                val dir = java.io.File(filesDir, "FocusKitty")
                dir.mkdirs()
                java.io.File(dir, "pending_watch.json").writeText(
                    """{"app_id":"$pkg","app_name":"$label","minutes":$minutes}"""
                )
            }
        }

        /** Ask for room above the cat for a line, or give it back. */
        @android.webkit.JavascriptInterface
        fun bubble(on: Boolean) {
            web?.post { setBubble(on) }
        }

        /** Walk the cat dx pixels along its edge, over ms milliseconds. */
        @android.webkit.JavascriptInterface
        fun stroll(dx: Int, ms: Int) {
            web?.post { stroll(dx, ms.toLong()) }
        }

        /** Drop a rule. The tracker removes it and the day's total together. */
        @android.webkit.JavascriptInterface
        fun stopWatch(pkg: String) {
            runCatching {
                val dir = java.io.File(filesDir, "FocusKitty")
                dir.mkdirs()
                java.io.File(dir, "pending_unwatch.json").writeText("""{"app_id":"$pkg"}""")
            }
        }

        /** Open the full app from the overlay's tools. */
        @android.webkit.JavascriptInterface
        fun openApp() {
            val i = Intent(this@CatOverlayService, MainActivity::class.java)
            i.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP)
            startActivity(i)
        }

        @android.webkit.JavascriptInterface
        fun collapse() {
            web?.post { if (expanded) toggle() }
        }

        @android.webkit.JavascriptInterface
        fun hideCat() {
            stopSelf()
        }
    }

    private fun buildNotification(): Notification {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val ch = NotificationChannel(
                CHANNEL, "FocusKitty",
                NotificationManager.IMPORTANCE_MIN
            ).apply { setShowBadge(false) }
            (getSystemService(NotificationManager::class.java)).createNotificationChannel(ch)
        }
        val open = PendingIntent.getActivity(
            this, 0, Intent(this, MainActivity::class.java),
            PendingIntent.FLAG_IMMUTABLE
        )
        val b = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O)
            Notification.Builder(this, CHANNEL) else
            @Suppress("DEPRECATION") Notification.Builder(this)
        return b.setContentTitle("FocusKitty is watching")
            .setContentText("Tap the cat for quick controls")
            .setSmallIcon(android.R.drawable.ic_menu_view)
            .setContentIntent(open)
            .setOngoing(true)
            .build()
    }

    override fun onDestroy() {
        isRunning = false
        web?.let { runCatching { wm.removeView(it) } }
        web?.destroy()
        web = null
        super.onDestroy()
    }
}

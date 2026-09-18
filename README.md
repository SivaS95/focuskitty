# FocusKitty

A cat that watches what you have in front of you, warns you before a limit,
and closes the tab when time is up. Then writes about you in its diary.

macOS first, Windows/Android/iOS to follow from the same Rust core.

## Status — phase 1 of 8 complete

The tracker works, headless, and is verified on real browsers. No cat yet.

| Phase | | |
|---|---|---|
| 0 | toolchain + scaffold | ✅ |
| 1 | headless tracker | ✅ verified |
| 2 | the cat — **art + rig done**, overlay/drag next | 🟡 |
| 3 | picker + limits + enforcement | |
| 4 | diary | |
| 5 | native app limits | |
| 6 | Windows | |
| 7 | Android | |
| 8 | iOS (cat-as-shield) | |

## Layout

```
crates/
  fk-core/         platform-independent: rules, timers, warnings, storage
  fk-probe-macos/  the only macOS-specific code: AppleScript + NSWorkspace
  fk-cli/          headless driver, for proving it works before any UI exists
ui/
  physics.js       springs, lag chains, irregular timers
  cat-svg.js       the cat: a side-facing skeleton with jointed limbs
  poses.js         the eight poses, as sets of joint targets
  cat.css          coats, the diary, sleep z's
  cat.js           the rig: poses, walk cycle, jaw, belly, idle behaviour
  preview.html     dev harness — drive every pose by hand
  poses.html       all eight poses side by side, for tuning geometry
```

`fk-core` knows nothing about macOS. Everything platform-specific hides behind
one trait, `ActivityProbe`, so Windows is one new impl rather than a new app.

## Watch the cat

Static images cannot show breathing, blinking, gaze-tracking or tail lag, so
look at it moving:

```sh
cd ui && python3 -m http.server 8777
# then open http://localhost:8777/preview.html
```

Move your pointer — it watches you. Leave it still for a couple of seconds and
it starts looking around on its own. Buttons drive every state and one-off
behaviour; `?state=writing&coat=ginger&bg=dark` also works as a URL.

### Poses

`sit · walk · sleep · pat · greet · bored · angry · confront`

A pose is a complete set of joint targets; springs blend between any two, so
transitions are free and never have to be authored. Coats: cream, white, black.

The cat is **side-facing with four jointed legs**, because a front-facing cat
cannot walk, curl up to sleep, or raise a paw. The sit geometry is derived
rather than eyeballed: the rear-to-shoulder span is 68.6 units and a leg is 54,
so the body must tilt `asin(54/68.6)` = 52 degrees for the front feet to reach
the floor, and the rear drops 45 units to rest on it.

### How it is animated

No sprite sheets, no keyframe loops, no WebGL. A looping animation reads as a
sticker, so nothing here loops:

- **Layered SVG in real CSS 3D.** Parts sit at different `translateZ`, so a
  head turn swings the nose further than the ears. Genuine parallax.
- **Springs, not tweens.** Every pose is a damped oscillator target, so motion
  accelerates, overshoots slightly and settles — that overshoot is most of what
  separates a creature from something being tweened.
- **A lagging tail chain.** Seven nested segments, each chasing its parent with
  decreasing stiffness, so the tip arrives late.
- **Irregular idle.** Blinks land somewhere between 2.6 s and 7.5 s, never on a
  beat. Yawn, stretch, groom, ear-flick and look-around are scheduled at random.
- **Gaze.** Pupils lead, head follows on a spring. Idle for long enough and it
  invents somewhere else to look.
- **Weight.** The contact shadow tightens and lightens as the cat lifts.
- **A jaw that opens.** The mouth is not a drawn line — it is a throat that
  widens with a tongue rising into it, so a yawn is a yawn. Fangs show when
  angry or confronting.
- **A belly that answers.** The stomach is its own shape, swelling harder on
  the breath than the chest does and buzzing visibly during a purr.

## Try it

```sh
cargo build --release

./target/release/focuskitty probe          # watch what it sees, live
./target/release/focuskitty tabs           # every open tab
./target/release/focuskitty apps           # apps you could limit
./target/release/focuskitty add youtube.com 30
./target/release/focuskitty watch          # DRY RUN — closes nothing
./target/release/focuskitty watch --live   # armed
```

`watch` is a dry run unless you pass `--live`. The app's main action destroys
something you did not back up, so it stays disarmed by default.

## What was measured, not assumed

| | |
|---|---|
| `current()`, non-browser in front | **402 ns** — the guard means zero Apple Events |
| `open_tabs()`, 34 tabs, naive | 1.14 s |
| `open_tabs()`, 34 tabs, bulk accessors | **181 ms** (6.3× faster) |
| `osascript` subprocess per call | ~180 ms, almost all process startup |
| in-process compiled script | the reason we do not shell out |

## Five things that bit, recorded so they do not bite again

**Addressing a browser launches it.** `tell application "Safari"` starts Safari
if it is not running. A focus app that opens Safari every ten seconds would be
intolerable, so every script call is gated behind an `NSWorkspace` running
check first — free, and needs no permission.

**OSAScript is main-thread only.** So `ActivityProbe` is deliberately *not*
`Send + Sync`. A probe that claimed to be shareable across threads would be
promising something macOS cannot deliver. The tick loop runs on the main
thread, which is where a 1 Hz sub-millisecond call belongs anyway.

**Unobserved time must not be charged.** If the lid was shut for eight hours,
that is not eight hours of Instagram — and it is not five seconds either.
A gap longer than `MAX_TICK_GAP_SECS` charges *nothing*, because we do not know
what happened while we were not looking.

**SVG `transform-origin` defaults to the viewBox origin, not the element.**
Scaling a pupil by 1.15 therefore also *translated* it about 15px, straight out
of its clip path — so pupils vanished in every state except idle, where the
scale happened to be exactly 1.0. Every scaled SVG part now states its origin
explicitly in viewBox units.

**A limb that must reach something has to bend, not swing.** The grooming paw
looked like pointing because the target was set by eye: the mouth is 35 units
from the shoulder but the leg is 50 long, so swinging it straight sent the paw
sailing past the head. Two-bone IK gives hip -98 / knee -91; guessing gave
-152 / -16. Same for the sit angle. Solve the geometry, do not eyeball it.

**Headless screenshots cannot drive rAF predictably.** `--virtual-time-budget`
advances the clock in a way that made four "different" frames of a timed action
come out identical, which nearly had me debugging an animation that was fine.
`Cat.step(dt)` and `solo.html?t=<seconds>` step the rig by hand instead, so a
screenshot lands on an exact moment every time. Steady poses screenshot fine;
timed actions do not.

**Tauri v2 denies every core plugin command until you write a capabilities
file — silently.** `invoke()` on your own commands keeps working, because app
commands are not gated; but `core:event:listen` is, so every event you emit
lands nowhere and the frontend just sits there. This one cause produced four
separate "bugs": cursor tracking dead, sleep/wake doing nothing, the Play
buttons inert, and the cat never writing. `src-tauri/capabilities/default.json`
is not optional. When frontend behaviour is missing rather than wrong, suspect
the ACL before the code.

**Make the webview say what it receives.** Several rounds were spent guessing at
IPC from the outside. A two-line `debug_ping` command, called once per event
stream, answered it immediately — the log showed `listen=function` but no events
arriving, which pointed straight at permissions.

**`start_dragging()` swallows the matching `pointerup`.** Calling it on
pointerdown left the cat wedged in `dragging = true` and the held pose forever,
deaf to every later pose change. Hand off to the host only after the pointer has
actually moved past a few pixels, so a plain click still completes.

**A side-on head cannot turn toward the viewer — so the face slides instead.**
Rotating a profile head just tips it over. The standard 2D cheat is to slide
every facial feature laterally across the skull and trade width between the
near and far ear; combined with a real pitch rotation that gives all four
directions of looking. Nudging the pupils a few pixels, which is what the first
version did, is invisible on screen.

**CSS 3D does not reach inside SVG.** A `rotateY` on an SVG `<g>` is silently
ignored, which is why the diary cover never opened. The book is real DOM now,
hinged on its spine, and genuinely swings.

*(A third, cheaper lesson: `var()` in a `stroke` presentation attribute
inherited from a parent `<g>` does not resolve, and paints nothing. The
whiskers spent a build invisible. Stroke from CSS rules instead.)*

## Background tabs

Audio is undetectable: neither Chrome's nor Safari's AppleScript dictionary
exposes `audible` or `muted`. So "is that YouTube tab actually playing" cannot
be answered without shipping a browser extension. What we *can* see is whether
the site is open at all, hence three per-rule modes:

| Mode | |
|---|---|
| `foreground-only` | time counts only while the tab is in front |
| `open-anywhere` | time counts whenever it is open in any tab |
| `foreground-notice` | counts in front only, but the cat comments (default) |

## Permissions

One Automation prompt per browser, the first time FocusKitty reads a tab.
Nothing else — `NSWorkspace` needs no permission at all. Denial degrades to
"tracking paused", never a crash.

// The rig.
//
// Joints are springs; poses are sets of spring targets. Blending between any
// two poses therefore costs nothing and always looks continuous.
//
// All SVG parts are posed by writing the `transform` ATTRIBUTE with an explicit
// pivot -- rotate(angle, cx, cy) -- never by CSS. SVG resolves CSS
// transform-origin against the viewBox rather than the element, so a CSS rotate
// quietly translates the part as well.

import { CAT_SVG, TAIL_SEGMENTS, tailSeg, RIG } from "./cat-svg.js";
import { POSES, STATE_POSE } from "./poses.js";
import { Spring, Chain, Irregular, smoothstep, lerp, rand } from "./physics.js";

const LEG_IDS = ["ff", "bf", "fn", "bn"];

/** Gait offsets: a diagonal walk, so opposite corners move together. */
const GAIT = { fn: 0, bf: 0.05, ff: 0.5, bn: 0.55 };

const set = (el, t) => el.setAttribute("transform", t);

export class Cat {
  constructor(mount, { coat = "cream", pose = "sit" } = {}) {
    mount.innerHTML = `<div class="stage">${CAT_SVG}</div>`;
    const $ = (id) => mount.querySelector("#" + id);
    this.mount = mount;

    this.el = {
      root: $("cat-root"), facing: $("facing"), body: $("cat-body-root"),
      torso: $("torso"), torsoShape: $("torso-shape"), shadow: $("shadow"),
      head: $("head-root"), earB: $("ear-back"), earF: $("ear-front"),
      eyeAOpen: $("eye-a-open"), eyeBOpen: $("eye-b-open"),
      eyeAShut: $("eye-a-shut"), eyeBShut: $("eye-b-shut"),
      muzzle: $("muzzle"), whiskers: $("whiskers"), belly: $("belly"),
      face: $("face"),
      mouthOpen: $("mouth-open"), tongue: $("tongue"),
      mouthClosed: $("mouth-closed"), mouthSmile: $("mouth-smile"),
      browA: $("brow-a"), browB: $("brow-b"),
      browASad: $("brow-a-sad"), browBSad: $("brow-b-sad"),
      fangL: $("fang-l"), fangR: $("fang-r"),
      blushA: $("blush-a"), blushB: $("blush-b"),
      tailRoot: $("tail-root"), zzz: $("zzz"),
      bowl: $("prop-bowl"), book: $("prop-book"), specs: $("glasses"),
      bubble: $("bubble"), bubbleText: $("bubble-text"),
      tail: Array.from({ length: TAIL_SEGMENTS }, (_, i) => $(`tail-${i}`)),
      legs: Object.fromEntries(LEG_IDS.map((id) => [id, {
        hip: $(`leg-${id}-hip`), knee: $(`leg-${id}-knee`),
      }])),
    };
    this.el.root.dataset.coat = coat;

    const sp = (k, d) => new Spring({ stiffness: k, damping: d });
    this.s = {
      bodyRot: sp(78, 13), bodyY: sp(88, 13), bodyX: sp(90, 14),
      torsoScale: sp(90, 13),
      headRot: sp(95, 13), headY: sp(100, 14), headX: sp(100, 14),
      earB: sp(150, 12), earF: sp(150, 12),
      eyes: sp(130, 15), tailBase: sp(80, 12), tailCurl: sp(80, 13),
      mouthOpen: sp(150, 14), mouthSmile: sp(140, 15), belly: sp(120, 13),
      brow: sp(150, 15), eyeScale: sp(140, 14), lick: sp(170, 15),
      headTurn: sp(105, 14), headPitch: sp(105, 13), twist: sp(80, 13),
      lift: sp(130, 14), gazeX: sp(150, 16), gazeY: sp(150, 16),
    };
    this.legS = Object.fromEntries(LEG_IDS.map((id) => [id, {
      hip: sp(110, 13), knee: sp(110, 13),
    }]));
    this.tailChain = new Chain(TAIL_SEGMENTS, { stiffness: 85, damping: 10, lag: 0.7 });

    this.t = 0;
    this.breathPhase = 0;
    this.tailPhase = 0;
    this.walkPhase = 0;
    this.x = 0;
    this.y = 0;
    this.dir = 1;
    this.facingScale = 1;
    this.facingStretch = 1;
    this.turning = false;
    this.dragging = false;
    /// True while something outside the rig is carrying the cat across the
    /// screen. The rig then walks on the spot and lets the host do the
    /// travelling -- see the locomotion block for why that matters.
    this.driven = false;
    this.blink = { t: -1, dur: 0.16, queued: 0 };
    this.action = null;
    this.gaze = { x: 0, y: 0 };
    this.autoGaze = { x: 0, y: 0 };
    this.pointerIdle = 0;

    this.blinker = new Irregular(2.6, 7, () => this.doBlink());
    this.idler = new Irregular(4.5, 11, () => this.randomIdle());
    this.glancer = new Irregular(3, 8, () => {
      if (this.pointerIdle > 2) this.autoGaze = { x: rand(-1, 1), y: rand(-.6, .5) };
    });

    this.pose = null;
    this.setPose(pose, true);

    this.running = true;
    this.last = performance.now();
    this._loop = this._loop.bind(this);
    requestAnimationFrame(this._loop);
  }

  // --- public -------------------------------------------------------------

  /** Blend to a pose. Springs do the interpolation, so this is instant to call. */
  setPose(name, immediate = false) {
    const p = POSES[name];
    if (!p) return;
    this.poseName = name;
    this.pose = p;

    this.s.bodyRot.target = p.bodyRot ?? 0;
    this.s.bodyY.target = p.bodyY ?? 0;
    this.s.bodyX.target = p.bodyX ?? 0;
    this.s.torsoScale.target = p.torsoScale ?? 1;
    this.s.headRot.target = p.headRot ?? 0;
    this.s.headY.target = p.headY ?? 0;
    this.s.headX.target = p.headX ?? 0;
    this.s.earB.target = p.earB ?? 0;
    this.s.earF.target = p.earF ?? 0;
    this.s.eyes.target = p.eyes ?? 1;
    this.s.mouthOpen.target = p.mouth?.[0] ?? 0;
    this.s.mouthSmile.target = p.mouth?.[1] ?? 0;
    this.s.belly.target = p.belly ?? 1;
    this.s.brow.target = p.brow ?? 0;
    this.s.eyeScale.target = p.eyeScale ?? 1;
    this.s.lick.target = 0;
    this.s.tailBase.target = p.tail?.[0] ?? 0;
    this.s.tailCurl.target = p.tail?.[1] ?? 0;
    for (const id of LEG_IDS) {
      this.legS[id].hip.target = p.legs?.[id]?.[0] ?? 0;
      this.legS[id].knee.target = p.legs?.[id]?.[1] ?? 0;
    }
    this.blinker.retune(...(p.blink ?? [2.6, 7]));

    this.el.root.dataset.pose = name;
    this.el.zzz.classList.toggle("show", !!p.zzz);
    // Props belong to the pose, so nothing can be left lying around.
    this.el.bowl.setAttribute("opacity", p.prop === "bowl" ? "1" : "0");
    this.el.book.setAttribute("opacity", p.prop === "book" ? "1" : "0");
    this.el.specs.setAttribute("opacity", p.specs ? "1" : "0");
    if (immediate) {
      for (const k in this.s) this.s[k].set(this.s[k].target);
      for (const id of LEG_IDS) {
        this.legS[id].hip.set(this.legS[id].hip.target);
        this.legS[id].knee.set(this.legS[id].knee.target);
      }
    } else {
      // A change of posture should read as a decision, not a fade.
      this.tailChain.impulse(rand(-14, -6));
      this.s.lift.impulse(rand(6, 14));
    }
  }

  /** Map the tracker's state onto a pose. */
  setState(name) {
    const pose = STATE_POSE[name];
    if (pose) this.setPose(pose);
  }

  /**
   * Look at something given in SCREEN space, and turn around if it is behind.
   *
   * A side-facing cat craning its neck at something over its shoulder looks
   * broken. Real cats turn to face what they are watching, and the rig already
   * has the mirror it needs -- it was only ever driven by the walk cycle.
   */
  lookAtScreen(dx, dy) {
    this.screenGaze = { x: dx, y: dy };

    // The head does the looking. The body is NOT spun round to chase the
    // cursor: squashing the silhouette through edge-on to mirror it is a
    // visible trick, and a cat watching something behind it twists at the
    // shoulder instead of pirouetting. The body only turns when it walks,
    // which is a turn with a reason behind it.
    this.lookAt(this.dir > 0 ? dx : -dx, dy);
  }

  /** Look toward a point, in roughly -1..1 relative to the head. */
  lookAt(x, y) {
    this.gaze.x = Math.max(-1.4, Math.min(1.4, x));
    this.gaze.y = Math.max(-1.2, Math.min(1.2, y));
    this.pointerIdle = 0;
  }

  followPointer(origin) {
    addEventListener("pointermove", (e) => {
      const o = origin();
      this.lookAt((e.clientX - o.x) / 190, (e.clientY - o.y) / 150);
    }, { passive: true });
  }

  say(text, ms = 4200) {
    this.el.bubbleText.textContent = text;
    this.el.bubble.classList.add("show");
    clearTimeout(this._bt);
    if (ms > 0) this._bt = setTimeout(() => this.hush(), ms);
    this.s.earB.impulse(-16); this.s.earF.impulse(16);
  }
  hush() { this.el.bubble.classList.remove("show"); }
  destroy() { this.running = false; clearTimeout(this._bt); }

  // --- one-off behaviours -------------------------------------------------

  doBlink(double = Math.random() < 0.2) {
    if (this.blink.t >= 0 || (this.pose?.eyes ?? 1) < 0.3) return;
    this.blink.t = 0;
    this.blink.queued = double ? 1 : 0;
  }

  randomIdle() {
    if (this.action) return;
    if (this.poseName === "sleep") return;           // let it sleep
    const opts = ["earFlick", "lookAround", "shiver", "yawn", "lick"];
    if (this.poseName === "sit") opts.push("stretch", "groom");
    this[opts[Math.floor(rand(0, opts.length))]]();
  }

  play(dur, update, onEnd) { this.action = { t: 0, dur, update, onEnd }; }

  earFlick() {
    Math.random() < 0.5 ? this.s.earF.impulse(58) : this.s.earB.impulse(-58);
  }

  shiver() { this.s.lift.impulse(7); this.tailChain.impulse(-10); }

  /**
   * Look around: up, down, back over the shoulder, then forward.
   *
   * The old version only nudged the pupils, so it was indistinguishable from
   * sitting still. This drives the head itself through four real positions,
   * holding each one long enough to register.
   */
  lookAround() {
    const seq = [
      { x: -1.0, y: -0.2 },   // back over the shoulder
      { x: -0.3, y: -1.0 },   // up
      { x: 0.9,  y:  0.25 },  // forward
      { x: 0.1,  y:  0.9 },   // down
      { x: 0,    y:  0 },     // back to centre
    ];
    this.play(5.2, (p) => {
      const i = Math.min(seq.length - 1, Math.floor(p * seq.length));
      this.autoGaze = seq[i];
      this.pointerIdle = 99;   // ignore a stale cursor for the duration
    });
  }

  /**
   * Stretch: reach FORWARD along the spine, not upward.
   *
   * The first version raised headY, which simply lifted the head off the neck
   * and left a gap. headX travels along the body axis instead, so the head
   * stays attached however the body is rotated.
   */
  stretch() {
    this.play(2.0, (p) => {
      const e = Math.sin(p * Math.PI);
      const base = this.pose;
      this.s.bodyRot.target = (base.bodyRot ?? 0) + e * 10;
      this.s.headX.target = (base.headX ?? 0) + e * 12;   // along the spine
      this.s.headRot.target = (base.headRot ?? 0) - e * 12;
      this.s.torsoScale.target = (base.torsoScale ?? 1) + e * 0.05;
      // Front legs reach out ahead; the rear stays planted.
      this.legS.fn.hip.target = base.legs.fn[0] - e * 52;
      this.legS.ff.hip.target = base.legs.ff[0] - e * 46;
      this.legS.fn.knee.target = base.legs.fn[1] + e * 16;
      this.legS.ff.knee.target = base.legs.ff[1] + e * 14;
      this.s.mouthOpen.target = Math.max(base.mouth?.[0] ?? 0, e * 0.3);
      if (p > 0.48 && p < 0.54) this.tailChain.impulse(-18);
    }, () => this.setPose(this.poseName, false));
  }

  /**
   * Groom: the paw comes all the way UP to the face and the head dips to meet
   * it. Swinging the leg a little and tilting the head -- which is all the
   * first version did -- leaves the two nowhere near each other.
   */
  groom() {
    this.play(2.6, (p) => {
      const env = smoothstep(Math.min(1, p * 3.2)) * smoothstep(Math.min(1, (1 - p) * 3.2));
      const lick = Math.sin(p * Math.PI * 9) * env;
      const base = this.pose;
      // Solved as two-bone IK, not guessed: the mouth is only 35 units from
      // the shoulder but the leg is 50 long, so it has to FOLD to get there.
      // Swinging it straight sends the paw sailing past the head instead.
      this.legS.fn.hip.target = base.legs.fn[0] - env * 98 + lick * 5;
      this.legS.fn.knee.target = base.legs.fn[1] - env * 91;
      // And the head comes down to it, which is what closes the gap.
      this.s.headRot.target = (base.headRot ?? 0) - env * 20 + lick * 4;
      this.s.headY.target = (base.headY ?? 0) + env * 9;
      this.s.headX.target = (base.headX ?? 0) - env * 4;
      this.s.eyes.target = (base.eyes ?? 1) * (1 - env * 0.55);
      this.s.lick.target = env * (0.55 + lick * 0.35);
    }, () => this.setPose(this.poseName, false));
  }

  /**
   * A real yawn: the jaw opens wide and stays open a beat, the head tilts
   * back, the eyes squeeze shut, the chest and belly swell with the breath.
   * Tilting the head alone -- which is all the first version did -- reads as
   * nothing at all.
   */
  yawn() {
    this.play(2.0, (p) => {
      // Slow open, hold at full, then snap shut. Yawns are not symmetrical.
      const open = p < 0.42 ? smoothstep(p / 0.42)
                 : p < 0.64 ? 1
                 : 1 - smoothstep((p - 0.64) / 0.36);
      const base = this.pose;
      this.s.mouthOpen.target = Math.max(base.mouth?.[0] ?? 0, open);
      this.s.eyes.target = (base.eyes ?? 1) * (1 - open * 0.95);
      this.s.headRot.target = (base.headRot ?? 0) - open * 24;
      this.s.headY.target = (base.headY ?? 0) - open * 4;
      this.s.earB.target = (base.earB ?? 0) - open * 14;
      this.s.earF.target = (base.earF ?? 0) + open * 12;
      this.s.torsoScale.target = (base.torsoScale ?? 1) + open * 0.035;
      this.s.belly.target = (base.belly ?? 1) + open * 0.12;
      if (p > 0.62 && p < 0.68) this.tailChain.impulse(-9);
    }, () => this.setPose(this.poseName, false));
  }

  /**
   * A lick: the tongue comes out and SWEEPS up over the nose, twice, with the
   * head following it. A tongue that only pokes straight out and back reads as
   * a glitch rather than a lick.
   */
  lick() {
    this.play(1.6, (p) => {
      const env = smoothstep(Math.min(1, p * 4)) * smoothstep(Math.min(1, (1 - p) * 4));
      const beat = Math.abs(Math.sin(p * Math.PI * 2.2));
      const base = this.pose;
      this.s.mouthOpen.target = Math.max(base.mouth?.[0] ?? 0, env * 0.42);
      this.s.lick.target = env * beat;
      // The sweep: the tongue arcs up across the muzzle rather than just out.
      this.lickSweep = env * Math.sin(p * Math.PI * 4.4);
      this.s.eyes.target = (base.eyes ?? 1) * (1 - env * 0.55);
      this.s.headRot.target = (base.headRot ?? 0) - env * 8 - beat * 5;
      this.s.headPitch.target = -env * 6;
    }, () => { this.lickSweep = 0; this.setPose(this.poseName, false); });
  }

  /**
   * The swipe: what actually closes your tab.
   *
   * Three beats, because a strike with no wind-up reads as a twitch:
   *   anticipation — weight rocks back, paw cocks, ears flatten, eyes narrow
   *   strike       — the paw lashes forward, the whole body follows it
   *   settle       — it drops back, tail lashing, pleased with itself
   *
   * The tab is closed on the strike frame, not when the timer expires, so the
   * cat is visibly the cause rather than a bystander reacting to it.
   */
  swipe() {
    const base = this.pose;
    this.play(0.9, (p) => {
      // reach: -1 fully cocked, +1.4 fully extended.
      let reach;
      if (p < 0.30) {
        reach = -smoothstep(p / 0.30);
      } else if (p < 0.46) {
        reach = -1 + 2.4 * smoothstep((p - 0.30) / 0.16);
      } else {
        reach = 1.4 * (1 - smoothstep((p - 0.46) / 0.54));
      }
      const back = Math.max(0, -reach);      // how cocked
      const fwd  = Math.max(0, reach);       // how extended

      // The striking paw.
      this.legS.fn.hip.target = base.legs.fn[0] + back * 26 - fwd * 66;
      this.legS.fn.knee.target = base.legs.fn[1] - back * 52 + fwd * 18;
      // The other front paw braces.
      this.legS.ff.hip.target = base.legs.ff[0] + back * 12 - fwd * 22;

      // The body goes with it, or the paw looks detached from the animal.
      this.s.bodyRot.target = (base.bodyRot ?? 0) + back * 7 - fwd * 9;
      this.s.bodyX.target = (base.bodyX ?? 0) - back * 3 + fwd * 5;
      this.s.headRot.target = (base.headRot ?? 0) + back * 6 - fwd * 12;
      this.s.headX.target = (base.headX ?? 0) + fwd * 4;

      // Face: narrowed in the wind-up, wide at the moment of contact.
      this.s.eyeScale.target = (base.eyeScale ?? 1) - back * 0.3 + fwd * 0.35;
      this.s.brow.target = Math.max(base.brow ?? 0, back * 0.9 + fwd * 0.5);
      this.s.earB.target = (base.earB ?? 0) - back * 30 - fwd * 10;
      this.s.earF.target = (base.earF ?? 0) + back * 26 + fwd * 8;
      this.s.mouthOpen.target = Math.max(base.mouth?.[0] ?? 0, fwd * 0.55);

      // Contact.
      if (!this.swipeHit && p >= 0.44) {
        this.swipeHit = true;
        this.tailChain.impulse(-46);
        this.s.lift.impulse(16);
      }
    }, () => {
      this.swipeHit = false;
      this.setPose(this.poseName, false);
      this.s.lift.target = 0;
    });
  }

  /**
   * Startle: a jump, a puff, eyes wide, ears flat, mouth open.
   *
   * The first version only nudged a few springs, which at this scale is
   * invisible. A fright has to be a pose change you cannot miss.
   */
  startle() {
    this.tailChain.impulse(-58);
    this.play(1.0, (p) => {
      // Snap up, then settle back down over the rest of the beat.
      const e = p < 0.16 ? smoothstep(p / 0.16) : Math.pow(1 - (p - 0.16) / 0.84, 2);
      const base = this.pose;
      this.s.lift.target = e * 30;
      this.s.torsoScale.target = (base.torsoScale ?? 1) + e * 0.14;   // fur puffs
      this.s.belly.target = (base.belly ?? 1) + e * 0.1;
      this.s.mouthOpen.target = Math.max(base.mouth?.[0] ?? 0, e * 0.6);
      this.s.eyeScale.target = (base.eyeScale ?? 1) + e * 0.55;       // eyes go wide
      this.s.earB.target = (base.earB ?? 0) - e * 46;
      this.s.earF.target = (base.earF ?? 0) + e * 42;
      this.s.headRot.target = (base.headRot ?? 0) - e * 12;
      this.s.bodyRot.target = (base.bodyRot ?? 0) - e * 8;
    }, () => { this.s.lift.target = 0; this.setPose(this.poseName, false); });
  }

  // --- frame --------------------------------------------------------------

  _loop(now) {
    if (!this.running) return;
    const dt = Math.min((now - this.last) / 1000, 1 / 20);
    this.last = now;
    if (!this.paused) this.update(dt);
    requestAnimationFrame(this._loop);
  }

  /**
   * Advance by a fixed step.
   *
   * Exposed so a timed behaviour can be inspected at an exact moment:
   * headless screenshots cannot drive rAF predictably, so verifying a yawn or
   * a groom by eye needs the clock in our hands rather than the browser's.
   */
  step(dt) { this.update(dt); }

  update(dt) {
    this.lastDt = dt;
    this.t += dt;
    this.pointerIdle += dt;

    this.blinker.update(dt);
    this.idler.update(dt);
    this.glancer.update(dt);

    if (this.action) {
      this.action.t += dt;
      const p = Math.min(1, this.action.t / this.action.dur);
      this.action.update(p);
      if (p >= 1) { this.action.onEnd?.(); this.action = null; }
    }

    this._walk(dt);
    this._quirks(dt);
    this._springs(dt);
    this._apply(dt);
  }

  /** The walk cycle, plus actually travelling. */
  _walk(dt) {
    if (!this.pose?.locomotion) { this.walkAmt = lerp(this.walkAmt ?? 0, 0, dt * 6); return; }
    this.walkAmt = lerp(this.walkAmt ?? 0, 1, dt * 5);
    this.walkPhase = (this.walkPhase + dt * 1.35) % 1;

    for (const id of LEG_IDS) {
      const ph = (this.walkPhase + GAIT[id]) % 1;
      const a = ph * Math.PI * 2;
      const back = id.startsWith("b");
      // Hip swings fore and aft; the knee only ever folds one way, which is
      // what stops the legs bending like a flamingo's.
      const hip = Math.sin(a) * (back ? 26 : 30);
      const knee = -Math.max(0, Math.sin(a - 0.9)) * (back ? 44 : 34);
      const base = this.pose.legs[id];
      this.legS[id].hip.target = base[0] + hip;
      this.legS[id].knee.target = base[1] + knee;
    }

    // Two bobs per stride, because two feet land per stride.
    this.bob = Math.abs(Math.sin(this.walkPhase * Math.PI * 2)) * 3.5;

    // Travel, and turn around at the edges. The cat art is 280 wide, so that
    // is what has to fit -- subtracting a guessed 200 let it walk off-screen.
    //
    // NOT while something else is carrying the cat. The overlay window is only
    // ~340 wide, so this span is about 30 pixels: left to itself the rig walks
    // that far, hits its own edge and turns round -- roughly every second and a
    // half. With the host moving the window at the same time the cat sets off
    // across the desk and then, halfway, walks backwards. On the spot is the
    // correct gait for an animal being carried somewhere.
    const span = (this.mount.clientWidth - 280) / 2;
    if (span > 6 && !this.dragging && !this.driven) {
      this.x += this.dir * dt * 38;
      if (this.x > span) { this.x = span; this.turnAround(); }
      if (this.x < -span) { this.x = -span; this.turnAround(); }
    }
  }

  /**
   * Face into the screen rather than off the edge of it.
   *
   * `atX` is where the cat sits, 0 (left edge) to 1 (right edge). Parked in the
   * right-hand corner it should look left, back across the desktop, because
   * everything worth watching is that way. Hysteresis around the middle stops
   * it dithering when placed near the centre.
   */
  faceInward(atX) {
    if (this.turning || this.dragging || this.pose?.locomotion) return;
    const want = atX > 0.55 ? -1 : atX < 0.45 ? 1 : this.dir;
    if (want !== this.dir) this.turnAround();
  }

  /**
   * Turn around, as an animation rather than a mirror.
   *
   * Used only when the cat WALKS the other way -- never to chase the cursor.
   * An instant `scale(-1,1)` is a cut; squashing all the way to edge-on is a
   * visible trick. This narrows partway, crosses over, and opens out again,
   * quickly enough to read as a body turning mid-stride.
   */
  turnAround() {
    if (this.turning) return;
    const from = this.facingScale ?? this.dir;
    this.turning = true;
    this.turnCooldown = this.t + 1.5;
    let crossed = false;

    this.s.earB.impulse(-24);
    this.s.earF.impulse(24);

    this.play(0.34, (p) => {
      const e = smoothstep(p);
      // Never fully edge-on: a silhouette squashed to zero width reads as the
      // picture glitching. Stopping at a third keeps it a body turning.
      const raw = from * (1 - 2 * e);
      this.facingScale = Math.sign(raw || from) * Math.max(0.34, Math.abs(raw));
      this.facingStretch = 1 + (1 - Math.abs(this.facingScale)) * 0.05;
      this.s.lift.target = Math.sin(p * Math.PI) * 8;

      // Direction of travel flips as the silhouette passes edge-on.
      if (!crossed && p >= 0.5) {
        crossed = true;
        this.dir = -this.dir;
        this.tailChain.impulse(this.dir * 26);
      }
    }, () => {
      this.facingScale = this.dir;
      this.facingStretch = 1;
      this.turning = false;
      this.s.lift.target = 0;
    });
  }

  // --- drag ---------------------------------------------------------------

  /**
   * Make the cat draggable, and make a plain click do something.
   *
   * Hit-testing is left to the SVG: pointer events land on drawn fill only, so
   * the transparent corners of the 280x210 box stay click-through and you can
   * only grab the animal itself.
   */
  enableDrag({ onMove, onClick, external = false } = {}) {
    const svg = this.el.root.querySelector(".cat-svg");
    svg.style.cursor = "grab";
    let start = null;

    /** Below this, it is a click; above it, a drag. */
    const SLOP = 4;

    const finish = () => {
      start = null;
      this.dragging = false;
      svg.style.cursor = "grab";
    };

    const down = (e) => {
      start = { px: e.clientX, py: e.clientY, x: this.x, y: this.y, moved: 0, handed: false };
      if (!external) svg.setPointerCapture?.(e.pointerId);
      e.preventDefault();
    };

    const move = (e) => {
      if (!start) return;
      const dx = e.clientX - start.px, dy = e.clientY - start.py;
      start.moved = Math.max(start.moved, Math.abs(dx) + Math.abs(dy));
      if (start.moved < SLOP) return;

      if (!this.dragging) {
        this.dragging = true;
        this.prevPose = this.poseName;
        this.setPose("held");
        svg.style.cursor = "grabbing";
      }

      if (external) {
        // Hand off to the host ONLY once a real drag has begun.
        //
        // Calling this on pointerdown swallows the matching pointerup -- the
        // window server takes the mouse -- which left the cat wedged in the
        // held pose forever and deaf to every later pose change.
        if (!start.handed) {
          start.handed = true;
          onMove?.("start");
          // pointerup may never arrive once the host owns the mouse.
          setTimeout(() => { if (start?.handed) { finish(); this.setPose(this.prevPose || "sit"); } }, 400);
        }
        return;
      }

      this.x = start.x + dx;
      this.y = start.y + dy;
      this.s.bodyRot.target = (POSES.held.bodyRot ?? 0) - Math.max(-18, Math.min(18, dx * 0.12));
      onMove?.(this.x, this.y);
    };

    const up = () => {
      if (!start) return;
      const wasClick = start.moved < SLOP;
      const prev = this.prevPose || "sit";
      const handed = start.handed;
      finish();

      if (wasClick) {
        onClick?.();
      } else if (!handed) {
        this.setPose(prev);
        this.s.lift.impulse(16);        // lands with a bump
        this.tailChain.impulse(-22);
      }
    };

    svg.addEventListener("pointerdown", down);
    addEventListener("pointermove", move, { passive: true });
    addEventListener("pointerup", up);
    addEventListener("pointercancel", up);
    // Losing the window mid-drag must not strand the cat in the held pose.
    addEventListener("blur", () => { if (this.dragging) { finish(); this.setPose(this.prevPose || "sit"); } });
  }

  /** Per-pose continuous behaviour that a static target cannot express. */
  _quirks(dt) {
    const p = this.pose;
    if (!p) return;

    if (p.wave) {
      // The raised paw actually waves.
      const w = Math.sin(this.t * 6.5) * 22;
      this.legS.fn.hip.target = p.legs.fn[0] + w * 0.4;
      this.legS.fn.knee.target = p.legs.fn[1] + w;
    }
    if (p.purr) {
      // A purr you can see: the whole cat hums, and the belly buzzes hardest.
      const buzz = Math.sin(this.t * 32) * 0.6;
      this.s.headX.target = (p.headX ?? 0) + buzz;
      this.s.bodyY.target = (p.bodyY ?? 0) + buzz * 0.5;
      this.s.belly.target = (p.belly ?? 1) + Math.sin(this.t * 30) * 0.045;
    }
    if (p.flick && Math.sin(this.t * 0.7) > 0.985) this.tailChain.impulse(-26);
    if (p.stalk) {
      const creep = Math.sin(this.t * 1.6) * 1.6;
      this.s.bodyX.target = (p.bodyX ?? 0) + creep;
    }
    if (p.dangle) {
      // Loose limbs swing on their own while the cat is held.
      const sway = Math.sin(this.t * 3.4);
      this.legS.fn.hip.target = p.legs.fn[0] + sway * 7;
      this.legS.bn.hip.target = p.legs.bn[0] - sway * 6;
      this.legS.ff.hip.target = p.legs.ff[0] + sway * 5;
      this.s.headRot.target = (p.headRot ?? 0) + sway * 4;
    }
    if (p.chew) {
      // Small, quick, irregular -- a cat eating is not a metronome.
      const bite = Math.sin(this.t * 9) + Math.sin(this.t * 3.7) * 0.5;
      this.s.headY.target = (p.headY ?? 0) + bite * 1.6;
      this.s.mouthOpen.target = Math.max(0, 0.18 + bite * 0.16);
      this.s.headRot.target = (p.headRot ?? 0) + bite * 2;
    }
    if (p.pageTurn) {
      // Mostly still, with an occasional turn of the page.
      const beat = Math.sin(this.t * 0.42);
      this.s.headRot.target = (p.headRot ?? 0) + Math.sin(this.t * 0.9) * 2.5;
      if (beat > 0.995) {
        this.legS.fn.hip.target = p.legs.fn[0] + 26;
        this.tailChain.impulse(-8);
      }
    }
    if (p.zzz) {
      // Sleep breathing is slow and deep, and the whole body rides it.
      this.s.bodyY.target = (p.bodyY ?? 0) + Math.sin(this.breathPhase * Math.PI * 2) * 1.6;
    }
  }

  _springs(dt) {
    for (const k in this.s) this.s[k].update(dt);
    for (const id of LEG_IDS) {
      this.legS[id].hip.update(dt);
      this.legS[id].knee.update(dt);
    }

    this.breathPhase += dt * (this.pose?.breath ?? 0.5);
    this.tailPhase += dt * (this.pose?.locomotion ? 1.5 : 0.5);

    if (this.blink.t >= 0) {
      this.blink.t += dt;
      if (this.blink.t / this.blink.dur >= 1) {
        this.blink.t = -1;
        if (this.blink.queued > 0) { this.blink.queued--; this.blink.t = 0; }
      }
    }
  }

  _apply(dt) {
    const s = this.s;
    const pivot = RIG.rear;

    // Whole body: rotate about the hip, so sitting lifts the chest correctly.
    const by = s.bodyY.value - s.lift.value + (this.bob ?? 0) * (this.walkAmt ?? 0);
    const tw = s.twist.value;
    set(this.el.body,
      `translate(${s.bodyX.value - tw * 7},${by}) ` +
      `rotate(${s.bodyRot.value - tw * 5},${pivot.x},${pivot.y})`);

    // Facing. A continuous scale, pivoted on the ground line so the feet stay
    // planted while the body turns through edge-on.
    const fx = this.facingScale ?? this.dir;
    const fy = this.facingStretch ?? 1;
    set(this.el.facing,
      `translate(140,${RIG.ground}) scale(${fx},${fy}) translate(-140,${-RIG.ground})`);

    // Breath rides on the torso only, so the legs do not inflate with it.
    const b = Math.sin(this.breathPhase * Math.PI * 2);
    const sc = s.torsoScale.value * (1 + b * 0.016);
    const twNarrow = 1 - Math.abs(tw) * 0.06;
    set(this.el.torso,
      `translate(128,124) scale(${sc * twNarrow},${sc}) translate(-128,-124)`);

    // Head, hinged at the neck, plus a little gaze.
    //
    // Unless the pose says the head is already committed. Gaze is ADDED to
    // the pose's headRot, so a sleeping cat went on turning and pitching its
    // head at the cursor, and the poses aimed at something -- the bowl, the
    // book -- were pulled off it. Easing the targets to zero rather than
    // freezing them lets the head settle into the pose and, on waking, pick
    // the cursor up again from wherever it now is.
    const still = !!this.pose?.noGaze;
    const blend = smoothstep((this.pointerIdle - 2) / 1.5);
    const gx = still ? 0 : lerp(this.gaze.x, this.autoGaze.x, blend);
    const gy = still ? 0 : lerp(this.gaze.y, this.autoGaze.y, blend);
    s.gazeX.target = gx; s.gazeY.target = gy;

    // Pitch: looking up and down is a real rotation of the whole head.
    s.headPitch.target = this.action ? s.headPitch.target : gy * 32;
    // Turn: a side-on head cannot rotate toward the viewer, so the FACE slides
    // across the skull and the two ears trade width. That is the standard 2D
    // cheat for a head turn, and it reads far better than nudging the pupils.
    // NOTE the sign. `gx` positive means "forward, the way the cat faces", and
    // forward is +x in the cat's own space. Negating it here turned the head
    // away from whatever it was supposed to be watching -- and the mirror then
    // mirrored the mistake, so it looked wrong in both directions.
    s.headTurn.target = Math.max(-1.35, Math.min(1.35, gx));

    const hr = s.headRot.value + s.headPitch.value;
    set(this.el.head,
      `translate(${RIG.head.x + s.headX.value},` +
      `${RIG.head.y + s.headY.value + b * 0.8}) rotate(${hr})`);

    const turn = Math.max(-1.35, Math.min(1.35, s.headTurn.value));

    // Looking well behind itself twists the shoulders and shifts the weight,
    // the way an animal does when something is over its shoulder.
    this.s.twist.target = Math.abs(turn) > 0.55 ? (turn - Math.sign(turn) * 0.55) : 0;
    // The skull is only 62 units across, so the face can travel about a fifth
    // of that before the eyes and muzzle slide off it entirely. Clamped hard:
    // the extra head range beyond this is expressed by rotation and the ears,
    // not by pushing the features further.
    const slide = Math.max(-1, Math.min(1, turn)) * 12;
    set(this.el.face,
      `translate(${slide},0) scale(${1 - Math.abs(turn) * 0.09},1)`);

    // Ears trade width as the head turns: the near one opens out, the far one
    // foreshortens. Without this the turn reads as the face sliding off.
    set(this.el.earB,
      `rotate(${s.earB.value},-14,-19) translate(-14,-19) ` +
      `scale(${1 + turn * 0.36},1) translate(14,19)`);
    set(this.el.earF,
      `rotate(${s.earF.value},15,-18) translate(15,-18) ` +
      `scale(${1 - turn * 0.36},1) translate(-15,18)`);

    // Legs.
    for (const id of LEG_IDS) {
      set(this.el.legs[id].hip, `rotate(${this.legS[id].hip.value},0,0)`);
      set(this.el.legs[id].knee,
        `translate(0,${RIG.thigh}) rotate(${this.legS[id].knee.value},0,0)`);
    }

    // Tail: pose curl + live sway, each segment lagging the last.
    const swayAmp = this.pose?.locomotion ? 9 : (this.poseName === "angry" ? 26 : 7);
    const drive = Math.sin(this.tailPhase * Math.PI * 2) * swayAmp +
                  Math.sin(this.tailPhase * Math.PI * 2 * 0.41 + 1.2) * swayAmp * 0.35;
    const chain = this.tailChain.update(drive, dt);
    set(this.el.tailRoot,
      `translate(${RIG.tailBase.x},${RIG.tailBase.y}) rotate(${s.tailBase.value})`);
    chain.forEach((a, i) => {
      const off = i === 0 ? 0 : -tailSeg(i - 1).len;
      set(this.el.tail[i], `translate(0,${off}) rotate(${s.tailCurl.value + a * (1 - i * 0.05)})`);
    });

    // Eyes: 1 open, 0 shut, anything between is a squint.
    let open = s.eyes.value;
    if (this.blink.t >= 0) {
      const p = this.blink.t / this.blink.dur;
      open *= 1 - smoothstep(p < 0.42 ? p / 0.42 : 1 - (p - 0.42) / 0.58);
    }
    open = Math.max(0, Math.min(1, open));
    for (const [o, c, cx, cy] of [
      [this.el.eyeAOpen, this.el.eyeAShut, -9, -2],
      [this.el.eyeBOpen, this.el.eyeBShut, 13, -3],
    ]) {
      const es = Math.max(0.05, s.eyeScale.value);
      set(o, `translate(${cx},${cy}) scale(${es},${Math.max(0.001, open * es)}) ` +
             `translate(${-cx},${-cy})`);
      c.setAttribute("opacity", String(1 - open));
    }

    // Mouth. The throat widens, the tongue rises into it, and the drawn
    // "w" fades out as the jaw parts -- otherwise both would show at once.
    const mo = Math.max(0, s.mouthOpen.value);
    const sm = Math.max(0, Math.min(1, s.mouthSmile.value));
    this.el.mouthOpen.setAttribute("ry", String(0.1 + mo * 9));
    this.el.mouthOpen.setAttribute("rx", String(7.5 + mo * 1.6));
    this.el.tongue.setAttribute("ry", String(mo * 4.2));
    this.el.tongue.setAttribute("cy", String(15 + mo * 4.5));
    this.el.mouthClosed.setAttribute("opacity", String((1 - Math.min(1, mo * 2.2)) * (1 - sm)));
    this.el.mouthSmile.setAttribute("opacity", String(sm * (1 - Math.min(1, mo * 2.2))));
    // The tongue can leave the mouth entirely, for licking and grooming.
    const lk = Math.max(0, s.lick.value);
    if (lk > 0.001) {
      this.el.tongue.setAttribute("ry", String(mo * 4.2 + lk * 5.5));
      this.el.tongue.setAttribute("cy", String(15 + mo * 4.5 + lk * 7));
      this.el.tongue.setAttribute("rx", String(4.6 + lk * 1.2));
      this.el.tongue.setAttribute("cx", String(2 + (this.lickSweep ?? 0) * 3.5));
    }

    const fang = this.pose?.fangs ? Math.min(1, mo * 1.8) : 0;
    this.el.fangL.setAttribute("opacity", String(fang));
    this.el.fangR.setAttribute("opacity", String(fang));

    // Belly: swells on the breath harder than the chest does, so breathing
    // reads in the stomach rather than as the whole cat inflating.
    const bel = s.belly.value * (1 + b * 0.055);
    set(this.el.belly, `translate(128,132) scale(${bel},${bel * (1 + b * 0.03)}) translate(-128,-132)`);

    // A bowl sits on the FLOOR. It lives inside the body group so it inherits
    // the cat's facing, but it must not inherit the body's rotation -- the
    // same mistake that had the diary floating beside the cat's head.
    if (this.pose?.prop === "bowl") {
      const inv =
        `rotate(${-s.bodyRot.value},${pivot.x},${pivot.y}) ` +
        `translate(${-s.bodyX.value},${-by}) `;
      set(this.el.bowl, inv);
    } else {
      set(this.el.bowl, "");
    }

    // Brows. Positive slopes them down toward the nose (angry); negative the
    // other way (droopy). Anger needs the eyes to change, not just the mouth.
    const br = s.brow.value;
    this.el.browA.setAttribute("opacity", String(Math.max(0, br)));
    this.el.browB.setAttribute("opacity", String(Math.max(0, br)));
    this.el.browASad.setAttribute("opacity", String(Math.max(0, -br)));
    this.el.browBSad.setAttribute("opacity", String(Math.max(0, -br)));

    // Blush deepens when content.
    const warm = this.poseName === "pat" || this.poseName === "greet" ? 0.85 : 0.45;
    this.el.blushA.setAttribute("opacity", String(warm));
    this.el.blushB.setAttribute("opacity", String(warm));

    // Position in the stage, and the shadow that keeps it on the ground.
    this.el.root.style.transform = `translate(${this.x}px, ${this.y}px)`;
    const lifted = Math.max(0, s.lift.value) + (this.bob ?? 0) * (this.walkAmt ?? 0);
    this.el.shadow.style.transform = `scale(${1 - lifted * 0.012},${1 - lifted * 0.03})`;
    this.el.shadow.style.opacity = String(Math.max(0.2, 0.85 - lifted * 0.03));


  }
}

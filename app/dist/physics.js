// Motion primitives. Everything the cat does is driven from these rather than
// from keyframes, because keyframes loop and looping reads as a sticker.

/**
 * A damped harmonic oscillator.
 *
 * Give it a target and it moves there with weight: it accelerates, overshoots
 * a little, and settles. That overshoot is most of what separates a creature
 * from a sprite being tweened.
 */
export class Spring {
  constructor({ stiffness = 120, damping = 14, value = 0 } = {}) {
    this.k = stiffness;
    this.d = damping;
    this.value = value;
    this.target = value;
    this.velocity = 0;
  }

  /** @param {number} dt seconds */
  update(dt) {
    // Clamped so a background tab that resumes after a long pause does not
    // explode the integration.
    const step = Math.min(dt, 1 / 30);
    const force = (this.target - this.value) * this.k;
    this.velocity += (force - this.velocity * this.d) * step;
    this.value += this.velocity * step;
    return this.value;
  }

  /** Knock it, as if something bumped the part. */
  impulse(v) {
    this.velocity += v;
  }

  set(v) {
    this.value = v;
    this.target = v;
    this.velocity = 0;
  }
}

/**
 * A chain of segments where each one lags the one before it.
 *
 * Used for the tail. A tail that rotates as a single rigid piece looks pinned
 * on; a tail whose tip arrives late looks attached to an animal.
 */
export class Chain {
  constructor(count, { stiffness = 90, damping = 11, lag = 0.55 } = {}) {
    this.segments = Array.from({ length: count }, (_, i) =>
      new Spring({
        // Further from the body = looser and slower, so the whip compounds.
        stiffness: stiffness * (1 - i * 0.12),
        damping: damping * (1 - i * 0.06),
      })
    );
    this.lag = lag;
  }

  /** Drive the root; every other segment chases its parent. */
  update(rootAngle, dt) {
    let parent = rootAngle;
    for (const seg of this.segments) {
      seg.target = parent * this.lag;
      seg.update(dt);
      parent = seg.value;
    }
    return this.segments.map((s) => s.value);
  }

  impulse(v) {
    this.segments.forEach((s, i) => s.impulse(v * (1 - i * 0.1)));
  }
}

/** Smooth 0..1 ramp. */
export const smoothstep = (t) => {
  const x = Math.max(0, Math.min(1, t));
  return x * x * (3 - 2 * x);
};

/** Ease with a little overshoot, for things that should feel springy but are one-shot. */
export const backOut = (t) => {
  const c = 1.70158 + 1;
  return 1 + c * Math.pow(t - 1, 3) + 1.70158 * Math.pow(t - 1, 2);
};

export const lerp = (a, b, t) => a + (b - a) * t;
export const rand = (min, max) => min + Math.random() * (max - min);

/**
 * Fires a callback on an irregular schedule.
 *
 * Deliberately never fixed-interval: a blink every 3.00s reads as a machine,
 * a blink somewhere between 2 and 7 seconds reads as an animal.
 */
export class Irregular {
  constructor(minSec, maxSec, fn) {
    this.min = minSec;
    this.max = maxSec;
    this.fn = fn;
    this.next = rand(minSec, maxSec);
    this.t = 0;
  }

  update(dt) {
    this.t += dt;
    if (this.t >= this.next) {
      this.t = 0;
      this.next = rand(this.min, this.max);
      this.fn();
    }
  }

  /** Change the cadence, e.g. blink faster when alarmed. */
  retune(minSec, maxSec) {
    this.min = minSec;
    this.max = maxSec;
    this.next = Math.min(this.next, maxSec);
  }
}

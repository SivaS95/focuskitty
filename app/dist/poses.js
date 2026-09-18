// Poses.
//
// A pose is a complete set of joint targets. Springs blend between them, so
// every transition is free and nothing has to be keyframed by hand.
//
// Leg angles are [hip, knee] in degrees. 0 is straight down; positive swings
// forward, toward the head. `tail` is [baseAngle, perSegment] and compounds
// through the chain, so a small per-segment value makes a long smooth curl.

// `noGaze` on a pose means the head belongs where the pose puts it and nowhere
// else. Gaze is added ON TOP of a pose's headRot, so without it a sleeping cat
// went on turning and pitching its head to follow the cursor, and the poses
// aimed at a prop -- eat at the bowl, read at the book -- were pulled off it.

export const POSES = {
  /**
   * Upright on the haunches.
   *
   * The numbers are derived, not eyeballed: the rear-to-shoulder span is 68.6
   * units and a leg is 54, so the body must tilt asin(54/68.6) = 52 degrees for
   * the front feet to reach the floor, and the rear must drop 45 units to rest
   * on it. The front legs then counter-rotate by the same 52 to hang vertical,
   * and the head does likewise so the face stays level.
   */
  sit: {
    bodyRot: -52, bodyY: 45,
    legs: { fn: [52, 0], ff: [49, 3], bn: [126, -142], bf: [122, -138] },
    headRot: 52, headX: 15, headY: 19, earB: 0, earF: 0,
    tail: [-38, 10],
    eyes: 1, breath: 0.5, blink: [2.6, 7],
    torsoScale: 1, mouth: [0, 0], belly: 1, brow: 0, eyeScale: 1,
  },

  /** On all fours, level, legs cycling. The cycle itself lives in the rig. */
  walk: {
    bodyRot: 0, bodyY: 14,
    legs: { fn: [0, 0], ff: [0, 0], bn: [0, 0], bf: [0, 0] },
    headRot: -4, headY: 6, earB: 2, earF: -2,
    tail: [-16, 4],
    eyes: 1, breath: 0.95, blink: [2.2, 5.5],
    torsoScale: 1, mouth: [0.12, 0], belly: 0.97, brow: 0, eyeScale: 1,
    locomotion: true,
  },

  /** Curled on its side, eyes shut, breathing slowly. */
  sleep: {
    bodyRot: 4, bodyY: 34,
    legs: { fn: [96, -128], ff: [90, -124], bn: [-84, -120], bf: [-78, -116] },
    headRot: 28, headX: -22, headY: 26, earB: -20, earF: 16,
    tail: [-96, 14],
    eyes: 0, breath: 0.24, blink: [99, 99],
    torsoScale: 1.04, mouth: [0.06, 0], belly: 1.08, brow: 0, eyeScale: 1,
    zzz: true, noGaze: true,
  },

  /** Being petted: head tilted up into the hand, eyes squeezed happy. */
  pat: {
    bodyRot: -52, bodyY: 45,
    legs: { fn: [52, 0], ff: [49, 3], bn: [126, -142], bf: [122, -138] },
    headRot: 30, headX: 13, headY: 9, earB: -12, earF: 10,
    tail: [-34, 11],
    eyes: 0.12, breath: 0.72, blink: [99, 99],
    torsoScale: 1, mouth: [0, 1], belly: 1.12, brow: 0, eyeScale: 1,
    purr: true,
  },

  /** Sitting up with the near front paw raised, waving. */
  greet: {
    bodyRot: -52, bodyY: 45,
    legs: { fn: [-42, -46], ff: [49, 3], bn: [126, -142], bf: [122, -138] },
    headRot: 44, headX: 16, headY: 14, earB: 6, earF: -6,
    tail: [-44, 12],
    eyes: 1, breath: 0.68, blink: [2, 5],
    torsoScale: 1, mouth: [0.18, 0.8], belly: 1, brow: 0, eyeScale: 1,
    wave: true,
  },

  /** Slumped, head low, tail flicking at nothing. */
  bored: {
    bodyRot: -46, bodyY: 42,
    legs: { fn: [46, 6], ff: [43, 8], bn: [124, -140], bf: [120, -136] },
    headRot: 60, headX: 10, headY: 26, earB: -18, earF: 14,
    tail: [-30, 7],
    eyes: 0.55, breath: 0.4, blink: [3.5, 9],
    torsoScale: 1, mouth: [0, 0], belly: 1.04, brow: -0.75, eyeScale: 0.95,
    flick: true,
  },

  /** Arched, ears flat, tail up and lashing. */
  angry: {
    bodyRot: -4, bodyY: -6,
    legs: { fn: [-12, 10], ff: [-18, 12], bn: [16, -14], bf: [22, -16] },
    headRot: 10, headY: 4, earB: -34, earF: 30,
    tail: [-54, -3],
    eyes: 1, breath: 1.5, blink: [6, 12],
    torsoScale: 1.1, mouth: [0.72, 0], fangs: true, belly: 0.92,
    brow: 1, eyeScale: 0.62,
    arch: true,
  },

  /** Low, forward, locked on. Not arched -- committed. */
  confront: {
    bodyRot: 4, bodyY: 16,
    legs: { fn: [-22, 16], ff: [-28, 18], bn: [34, -40], bf: [40, -44] },
    headRot: 16, headY: 14, headX: 10, earB: -28, earF: 24,
    tail: [-8, 1],
    eyes: 1, breath: 1.15, blink: [7, 14],
    torsoScale: 1, mouth: [0.22, 0], fangs: true, belly: 0.96,
    brow: 0.8, eyeScale: 0.74,
    stalk: true,
  },
};

/**
 * Mid-air: stretched out, legs trailing, tail streaming behind.
 *
 * Used for the leap onto a tab. Sliding the sitting pose through the air is
 * what made the cat look like it was flying rather than jumping.
 */
POSES.leap = {
  bodyRot: -14, bodyY: -6,
  legs: { fn: [-54, -18], ff: [-44, -14], bn: [46, -30], bf: [38, -26] },
  headRot: -10, headY: -4, earB: -18, earF: 14,
  tail: [-62, 2],
  eyes: 1, breath: 1.6, blink: [9, 14],
  torsoScale: 1.04, mouth: [0.3, 0], belly: 0.94,
  brow: 0.35, eyeScale: 1.25,
};

/**
 * Landed: absorbing the impact, before standing back up.
 */
POSES.land = {
  bodyRot: -6, bodyY: 20,
  legs: { fn: [24, -34], ff: [18, -30], bn: [70, -84], bf: [64, -78] },
  headRot: 14, headY: 8, earB: -10, earF: 8,
  tail: [-20, 6],
  eyes: 1, breath: 1.4, blink: [6, 11],
  torsoScale: 0.95, mouth: [0.18, 0], belly: 1.06,
  brow: 0.2, eyeScale: 1.1,
};

/**
 * Eating. Head right down to the bowl, which a sitting cat cannot reach --
 * the body has to tip forward and the front end drop for the nose to get there.
 */
POSES.eat = {
  // bodyY 30 put the head 13px THROUGH the floor -- the face went under the
  // bowl instead of into it. At 0 the muzzle meets the rim, and the legs fold
  // to keep the paws on the ground while the front end is tipped down.
  bodyRot: 18, bodyY: 0,
  legs: { fn: [30, -100], ff: [24, -94], bn: [-18, -4], bf: [-13, -7] },
  headRot: 30, headX: 4, headY: 10, earB: -12, earF: 10,
  tail: [-24, 5],
  eyes: 0.45, breath: 0.7, blink: [2.4, 6],
  torsoScale: 1, mouth: [0.3, 0], belly: 1.05,
  brow: 0, eyeScale: 1, prop: "bowl", chew: true, noGaze: true,
};

/**
 * Reading, in spectacles. The book is held at chest height, which is the one
 * place the front paws can actually reach.
 */
POSES.read = {
  // Paw angles solved so the book lands in front of the FACE, not down at the
  // chest: the head sits at world (144,82), so the book belongs around
  // (170,118) -- 37 units from the shoulder, well inside the leg's 50.
  bodyRot: -46, bodyY: 42,
  legs: { fn: [-14, -86], ff: [-8, -80], bn: [126, -142], bf: [122, -138] },
  headRot: 40, headX: 10, headY: 14, earB: -6, earF: 4,
  tail: [-30, 9],
  eyes: 0.72, breath: 0.42, blink: [3, 8],
  torsoScale: 1, mouth: [0, 0], belly: 1,
  brow: 0, eyeScale: 1, prop: "book", specs: true, pageTurn: true, noGaze: true,
};

/**
 * Held: picked up and dangling.
 *
 * Legs hang loose rather than standing on anything, the tail drops, the ears
 * go back and the eyes widen. A cat that keeps its sitting pose while you drag
 * it around reads as a sticker being moved, not an animal being carried.
 */
POSES.held = {
  bodyRot: -8, bodyY: 6,
  legs: { fn: [-14, 22], ff: [-8, 26], bn: [18, 30], bf: [24, 34] },
  headRot: 12, headX: 4, headY: -2, earB: -22, earF: 18,
  tail: [18, 4],
  eyes: 1, breath: 1.25, blink: [3, 7],
  torsoScale: 1.02, mouth: [0.2, 0], belly: 1.02,
  brow: 0, eyeScale: 1.18,
  dangle: true,
};

/** Poses the app itself drives, mapped from tracker state. */
export const STATE_POSE = {
  idle: "sit",
  writing: "sit",
  warning: "confront",
  alarmed: "angry",
  pleased: "pat",
};

export const POSE_NAMES = Object.keys(POSES);

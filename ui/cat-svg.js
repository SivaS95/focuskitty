// The cat: a side-facing quadruped with a real skeleton.
//
// Front-facing art cannot walk, sleep curled, or raise a paw to greet, so the
// rig is built side-on with four jointed legs, a jointed neck and a tail chain.
//
// Every moving part is rotated by WRITING THE SVG `transform` ATTRIBUTE with an
// explicit centre -- `rotate(angle, cx, cy)` -- rather than by CSS. SVG resolves
// CSS `transform-origin` against the viewBox, not the element, so a CSS rotate
// silently translates the part as well. The attribute form takes its pivot
// inline and cannot drift.

export const TAIL_SEGMENTS = 8;

/** Joint pivots, in viewBox units. The rig and the art read from this. */
export const RIG = {
  /** Where the feet land when standing. Everything else is derived from it. */
  ground: 185,
  /** Sitting rotates about the rear, which is what touches the floor. */
  rear:     { x: 92,  y: 140 },
  hip:      { x: 98,  y: 131 },
  shoulder: { x: 160, y: 131 },
  head:     { x: 190, y: 94  },
  tailBase: { x: 86,  y: 122 },
  legs: {
    // near = viewer's side; far = the other two, drawn behind and darkened
    ff: { x: 152, y: 128, far: true  },
    bf: { x: 104, y: 128, far: true  },
    fn: { x: 162, y: 131, far: false },
    bn: { x: 98,  y: 131, far: false },
  },
  thigh: 26,
  shin: 24,
};

export const tailSeg = (i) => ({
  w: 9 - i * 0.78,
  len: 11 - i * 0.5,
});

const leg = (id, { x, y, far }) => `
  <g id="leg-${id}" transform="translate(${x},${y})" ${far ? 'class="leg-far"' : ""}>
    <g id="leg-${id}-hip">
      <rect x="-5.5" y="0" width="11" height="${RIG.thigh + 3}" rx="5.5" fill="var(--limb)"/>
      <g id="leg-${id}-knee" transform="translate(0,${RIG.thigh})">
        <rect x="-4.8" y="0" width="9.6" height="${RIG.shin + 2}" rx="4.8" fill="var(--limb)"/>
        <g id="leg-${id}-paw" transform="translate(0,${RIG.shin})">
          <ellipse cx="1.5" cy="2" rx="8" ry="5.5" fill="var(--paw)"/>
        </g>
      </g>
    </g>
  </g>`;

const tail = () => {
  let open = "";
  let close = "";
  for (let i = 0; i < TAIL_SEGMENTS; i++) {
    const { w, len } = tailSeg(i);
    open += `
      <g id="tail-${i}" transform="translate(0,${i === 0 ? 0 : -tailSeg(i - 1).len})">
        <rect x="${-w / 2}" y="${-len}" width="${w}" height="${len + 3}" rx="${w / 2}"
              fill="var(--fur)"/>`;
    close = `</g>` + close;
  }
  open += `<ellipse cx="0" cy="-10" rx="5.2" ry="6" fill="var(--tail-tip)"/>`;
  return open + close;
};

const DEFS = `
<defs>
  <radialGradient id="bodyG" cx="60%" cy="28%" r="80%">
    <stop offset="0%"   stop-color="var(--fur-hi)"/>
    <stop offset="62%"  stop-color="var(--fur)"/>
    <stop offset="100%" stop-color="var(--fur-dark)"/>
  </radialGradient>
  <radialGradient id="headG" cx="62%" cy="26%" r="78%">
    <stop offset="0%"   stop-color="var(--fur-hi)"/>
    <stop offset="60%"  stop-color="var(--fur)"/>
    <stop offset="100%" stop-color="var(--fur-dark)"/>
  </radialGradient>
  <clipPath id="clipHead"><circle cx="0" cy="0" r="31"/></clipPath>
</defs>`;

export const CAT_SVG = `
<div class="cat-root" id="cat-root">
  <div class="shadow" id="shadow"></div>

  <svg class="cat-svg" id="cat-svg" viewBox="0 0 280 210">
    ${DEFS}

    <!-- Facing group: flipped when the cat turns around while walking. -->
    <g id="facing">
      <!-- Everything that moves as one when walking. -->
      <g id="cat-body-root">

        <!-- Tail, behind everything. -->
        <g id="tail-root" transform="translate(${RIG.tailBase.x},${RIG.tailBase.y})">
          ${tail()}
        </g>

        <!-- Far legs, behind the torso. -->
        ${leg("bf", RIG.legs.bf)}
        ${leg("ff", RIG.legs.ff)}

        <!-- Torso. Rotates to sit, arches to threaten, curls to sleep. -->
        <g id="torso">
          <path id="torso-shape"
                d="M100 100
                   C 124 92, 152 94, 168 106
                   C 178 115, 176 132, 166 140
                   C 150 151, 106 151, 92 141
                   C 80 132, 82 108, 100 100 Z"
                fill="url(#bodyG)"/>
          <!-- The belly is its own shape so it can swell, sag and vibrate
               without dragging the chest or the legs with it. A purr has to be
               visible in the stomach or it does not read as a purr. -->
          <ellipse id="belly" cx="128" cy="132" rx="33" ry="15" fill="var(--fur-light)"
                   opacity=".85"/>
        </g>

        <!-- Head, hinged at the neck. -->
        <g id="head-root" transform="translate(${RIG.head.x},${RIG.head.y})">
          <g id="ear-back">
            <path d="M-25 -13 Q -33 -44 -22 -46 Q -10 -40 -3 -25 Z" fill="var(--fur-dark)"/>
            <path d="M-23 -17 Q -28 -38 -21 -39 Q -13 -34 -8 -24 Z" fill="var(--ear-inner)"/>
          </g>
          <g id="ear-front">
            <path d="M3 -24 Q 12 -53 23 -50 Q 30 -40 27 -11 Z" fill="var(--fur)"/>
            <path d="M7 -25 Q 14 -46 22 -44 Q 26 -36 24 -16 Z" fill="var(--ear-inner)"/>
          </g>

          <circle cx="0" cy="0" r="31" fill="url(#headG)"/>

          <g id="face">
          <!-- Cheek blush -->
          <ellipse id="blush-a" cx="-14" cy="10" rx="8" ry="5" fill="var(--blush)" opacity=".55"/>
          <ellipse id="blush-b" cx="20"  cy="9"  rx="7.5" ry="4.5" fill="var(--blush)" opacity=".55"/>

          <!-- Eyes: simple, dark, expressive. Two shapes each so they can
               switch between open, squinting and shut without redrawing. -->
          <g id="eye-a">
            <ellipse id="eye-a-open" cx="-9" cy="-2" rx="4.4" ry="5.6" fill="var(--eye)"/>
            <circle cx="-10.6" cy="-4" r="1.7" fill="#fff" opacity=".9"/>
            <path id="eye-a-shut" d="M-14 -2 q 5 5 10 0" stroke="var(--eye)" stroke-width="2.2"
                  fill="none" stroke-linecap="round" opacity="0"/>
          </g>
          <g id="eye-b">
            <ellipse id="eye-b-open" cx="13" cy="-3" rx="4.4" ry="5.6" fill="var(--eye)"/>
            <circle cx="11.4" cy="-5" r="1.7" fill="#fff" opacity=".9"/>
            <path id="eye-b-shut" d="M8 -3 q 5 5 10 0" stroke="var(--eye)" stroke-width="2.2"
                  fill="none" stroke-linecap="round" opacity="0"/>
          </g>

          <g id="glasses" opacity="0">
            <circle cx="-9" cy="-2" r="11" fill="#cfe4ff" opacity=".28"/>
            <circle cx="13" cy="-3" r="11" fill="#cfe4ff" opacity=".28"/>
            <circle cx="-9" cy="-2" r="11" fill="none" stroke="var(--specs)" stroke-width="2.2"/>
            <circle cx="13" cy="-3" r="11" fill="none" stroke="var(--specs)" stroke-width="2.2"/>
            <path d="M2 -2.5 h0" stroke="var(--specs)" stroke-width="2.2" stroke-linecap="round"/>
            <path d="M-20 -3 L-30 -7" stroke="var(--specs)" stroke-width="2" stroke-linecap="round"/>
            <path d="M24 -4 L33 -8" stroke="var(--specs)" stroke-width="2" stroke-linecap="round"/>
          </g>

          <g id="brows">
            <path id="brow-a"     class="brow" d="M-18 -14 L -3 -8"  opacity="0"/>
            <path id="brow-b"     class="brow" d="M22 -15 L 7 -9"    opacity="0"/>
            <path id="brow-a-sad" class="brow" d="M-18 -8 L -3 -14"  opacity="0"/>
            <path id="brow-b-sad" class="brow" d="M22 -9 L 7 -15"    opacity="0"/>
          </g>

          <!-- Muzzle. The mouth genuinely opens: a throat that widens, a
               tongue inside it, and the closed "w" fading out as it does. A
               yawn that only tilts the head is not a yawn. -->
          <g id="muzzle">
            <g id="jaw">
              <ellipse id="mouth-open" cx="2" cy="15" rx="7.5" ry="0.1" fill="var(--throat)"/>
              <ellipse id="tongue" cx="2" cy="18" rx="4.6" ry="0.1" fill="var(--tongue)"/>
              <path id="fang-l" d="M-3.6 11 l1.7 3 1.7 -3 z" fill="#fff" opacity="0"/>
              <path id="fang-r" d="M4.2 11 l1.7 3 1.7 -3 z" fill="#fff" opacity="0"/>
            </g>
            <path id="nose" d="M2 6 l4 3 -4 3 -4 -3 z" fill="var(--nose)"/>
            <path id="mouth-closed" d="M2 12 q -4 5 -8 1 M2 12 q 4 5 8 1"
                  stroke="var(--line)" stroke-width="1.8" fill="none" stroke-linecap="round"/>
            <path id="mouth-smile" d="M-6 11 q 8 7 16 0"
                  stroke="var(--line)" stroke-width="1.8" fill="none"
                  stroke-linecap="round" opacity="0"/>
          </g>

          <g id="whiskers">
            <path class="w-edge" d="M-8 8 Q -26 4 -40 0"/>
            <path class="w-edge" d="M-8 12 Q -26 14 -42 14"/>
            <path class="w-edge" d="M12 8 Q 28 4 42 1"/>
            <path class="w-edge" d="M12 12 Q 28 14 44 15"/>
            <path class="w-line" d="M-8 8 Q -26 4 -40 0"/>
            <path class="w-line" d="M-8 12 Q -26 14 -42 14"/>
            <path class="w-line" d="M12 8 Q 28 4 42 1"/>
            <path class="w-line" d="M12 12 Q 28 14 44 15"/>
          </g>
          </g><!-- /face -->
        </g>

        <!-- Props. Hidden unless a pose asks for them; kept in the rig so
             they inherit the cat's facing and never drift away from it. -->
        <g id="prop-bowl" opacity="0">
          <ellipse cx="214" cy="180" rx="26" ry="7" fill="#000" opacity=".12"/>
          <path d="M190 168 h48 a4 4 0 0 1 -4 10 h-40 a4 4 0 0 1 -4 -10 z"
                fill="var(--bowl)"/>
          <ellipse cx="214" cy="168" rx="24" ry="6" fill="var(--bowl-rim)"/>
          <ellipse cx="214" cy="167" rx="18" ry="4" fill="var(--kibble)"/>
          <circle cx="206" cy="166" r="2.6" fill="var(--kibble-dark)"/>
          <circle cx="215" cy="168" r="2.4" fill="var(--kibble-dark)"/>
          <circle cx="222" cy="165" r="2.2" fill="var(--kibble-dark)"/>
        </g>

        <!-- Near legs, in front of the torso. -->
        ${leg("bn", RIG.legs.bn)}
        ${leg("fn", RIG.legs.fn)}

        <g id="prop-book" opacity="0">
          <g transform="translate(192,152) rotate(20) scale(0.92)">
            <!-- Coloured cover, or the pages vanish against a pale cat. -->
            <path d="M0 4 L34 -14 L40 4 L3 18 Z" fill="var(--cover)"/>
            <path d="M0 4 L-34 -14 L-40 4 L-3 18 Z" fill="var(--cover-dark)"/>
            <!-- Pages, inset from the cover so a spine edge shows. -->
            <path d="M1 2 L30 -12 L34 1 L3 13 Z" fill="var(--page)"/>
            <path d="M-1 2 L-30 -12 L-34 1 L-3 13 Z" fill="var(--page)"/>
            <path d="M0 1 L1 15" stroke="var(--cover-dark)" stroke-width="1.6"/>
            <g stroke="var(--ink)" stroke-width="1.2" opacity=".5" stroke-linecap="round">
              <path d="M-26 -7 L-9 0"/><path d="M-28 -3 L-7 5"/>
              <path d="M8 0 L25 -7"/><path d="M6 5 L27 -3"/>
            </g>
          </g>
        </g>

      </g>
    </g>
  </svg>

  <!-- Sleep z's -->
  <div class="zzz" id="zzz"><i>z</i><i>z</i><i>z</i></div>

  <div class="bubble-layer" id="bubble">
    <div class="bubble-tail-dot d1"></div>
    <div class="bubble-tail-dot d2"></div>
    <div class="bubble-body"><span id="bubble-text"></span></div>
  </div>
</div>
`;

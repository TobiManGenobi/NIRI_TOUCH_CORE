# niri-toublet — design

A niri fork with a first-class touchscreen layer: simpler than either
existing fork, more stable, and configurable enough to be a real
touch-based compositor rather than a desktop with gestures bolted on.

## What this is built from

Two prior implementations, both on niri `v26.04`:

| | niri-touch (`feat/touch`) | niri-tablet (20-patch series) |
|---|---|---|
| Shape | whole-tree fork | patch series vs upstream |
| Config model | unified `binds {}`, parameterized triggers | fixed `gestures {}` nodes |
| Recognizer | `src/input/touch_gesture.rs` (2424 ln) | `touch_swipe_tracker.rs` (923), `touch_edge_swipe.rs` (515), `touch_swipe_grab.rs` (399) |
| App arbitration | `touchscreen-gesture-passthrough` window rule | none |
| Pinch / rotate | yes | no |
| Corner zones | no | yes (8 zones) |
| Tests | 23 + 45 | 86 |

Neither is a superset. The merged design takes the best of each and
discards both ownership models.

## The problem with both

Gesture *recognition* is not what breaks. What breaks is that all three
concerns — recognise, arbitrate, act — are fused into niri's touch-down
hot path, against an input stack whose grab semantics are the opposite of
what gesture logic naturally assumes.

Evidence:

- niri-touch puts ~1400 lines of gesture logic inline in the compositor's
  `State` (`touch_gesture.rs` lines 104–1530).
- niri-tablet needed **4 of 20 patches** purely to fix the same subsystem:
  `0006` late-joining fingers, `0010` sequence ownership, `0011` smithay
  grab seam, `0012` device-removal state.
- **14 of 20** niri-tablet patches touch `src/input/mod.rs` — the
  rebase-conflict surface.

Patch `0011` is the clearest statement of the hazard:

> every finger of the gesture was delivered to the client … Clients were
> left with stuck touch points: browsers pinch-zoomed from a single finger
> and ignored taps after multi-finger gestures … up to 9 phantom touches
> open at once

Root cause: smithay installs an implicit `TouchDownGrab` on every delivered
touch-down, so `handle.is_grabbed()` is *always* true over app surfaces.
Both forks' ownership logic assumes the opposite, so both shipped a band-aid
(`if !is_grabbed()`) instead of a model.

**Getting the ownership model right is the whole difference** between a
compositor that works on your device and one that intermittently eats taps.

## Architecture — three layers, one-way dependencies

```
┌─ L3  ARBITRATION ──────────────────────────────────┐
│  Who owns this touch? Pure fn of (down-context, cfg) │
│  window rules · exclusion rects · Mod · pen · device │
├─ L2  RECOGNITION ──────────────────────────────────┤
│  Pure state machines. No compositor, no config.     │
│  ContactFrame* →  GestureBegin/Update/End          │
├─ L1  CONTACT ─────────────────────────────────────┐
│  Slots · centroid · spread · angle · edge zone      │
│  No policy.                                         │
└─────────────────────────────────────────────────────┘
```

The property that buys stability: **L2 is headless.** Feed it recorded
contact traces, assert on gesture output. The bug class that produced
`0010`/`0011`/`0012` — client-visible touch bookkeeping — lives in L3,
which becomes small and testable with fakes instead of a live seat.

L1 and L2 ship as `crates/touch-core`: **zero dependencies**, no smithay, no
`niri-config`. That is what lets `cargo test` run the gesture core in
milliseconds instead of building a compositor.

## Config — one grammar

`tap-more` / `swipe-more-up` encode "one finger above base" as a permanent
part of every node name. That does not scale past one tier, and it makes a
3-finger tap and a 5-finger tap mutually exclusive. One keyword, orthogonal
properties instead:

```kdl
gestures {
    sensitivity 0.4
    edge-zone 48
    tap-wobble 24

    // ── policy: the actual tablet feature ────────────
    policy default compositor
    policy window /app-id="org.kde.krita" client {
        exclude-rect top 12%          // Android-style
    }

    // ── one shape for every gesture ──────────────────
    bind Swipe   fingers=3 direction=down    { close-window }
    bind Swipe   fingers=3 direction=right   { resize-column-live }
    bind Tap     fingers=4                   { toggle-overview }
    bind Tap     fingers=3 hold direction=left  { move-column-left }
    bind Pinch   fingers=2 direction=in       { zoom-in }
    bind Rotate  fingers=2 direction=cw      { rotate-window }
    bind Edge    edge=bottom                 { toggle-osk }
    bind Edge    edge=top zone=left          { toggle-overview }
    bind Corner  corner=top-left             { toggle-overview }
}
```

`fingers` accepts any `1..=10` uniformly. `zone` and `corner` are
properties, not separate node families.

Keeps niri-tablet's best idea (discoverable grouping; every action node
takes any niri action) and niri-touch's best idea (parameterisation, no tier
encoding).

## Zone model — union of both forks

Neither fork is complete here, so L1 classifies into a superset:

- niri-tablet: `SwipeZone` 8 variants (4 edges + 4 corners), `EDGE_ZONE_PX = 48.0`
- niri-touch: `TouchEdge { edge, zone }` — 4 edges × 3 thirds, parent fallback

```rust
enum Zone {
    Edge  { edge: Edge, third: Third },
    Corner { corner: Corner },
}
```

Corner wins over edge when a point is in both. That matches niri-tablet's
behaviour and is deterministic rather than order-dependent.

## What a real touch compositor needs that neither has

**1. Pen as a separate input class.** The largest hole — `grep` for
`stylus` / `pressure` / `tilt` returns nothing in either recognizer. Today a
stylus is a finger that happens to have buttons. It needs: pen-down must
never arm a multi-finger gesture; barrel button acts as Mod during a gesture;
eraser tip is a distinct tool; palm rejection is scoped to the pen's contact
radius. niri already has `TabletStylusButton1/2/3` upstream; neither fork
wires them into gesture arbitration.

**2. Rect-based exclusion, not a bool.** niri-touch's bool is right for v1
and wrong for the browser case: "forward my pinch, but keep your edge
swipes." Android's `systemGestureExclusionRects` has shipped for six years.

**3. A minimal exclusion protocol.** Client advertises per-surface rects.
This is niri-touch design doc §7.1's own conclusion — and the one protocol
worth writing, because it makes the bool unnecessary over time. Note the
inverse of niri-touch's `niri_touch_gesture_v1`: do **not** forward gestures
*to* clients; ask them where to stay out. Forwarding contradicts the
ownership model that makes niri-tablet's gestures feel solid at all.

**4. Learnability HUD.** niri-tablet's compositor-drawn touch points are the
right substrate and a genuine advantage — they survive screen recording. Keep
them, and add one layer: name the gesture as performed, with a visible undo
affordance for destructive ones.

## Stability rules

Non-negotiable, because these are the exact failure modes in patch `0011`:

- **Invariant: a client-visible touch is never left open when a gesture grab
  installs.** Enforce with a debug assertion plus a regression test, not a
  comment.
- **One `Ownership` enum, one transition function, exhaustive `match`.** No
  `is_grabbed()` heuristics. Smithay's implicit-grab semantics are handled
  once, in one place, and tested.
- **Golden-trace tests.** ~200 recorded contact traces → asserted gesture
  streams. Cover late-joining fingers, out-of-order lift, palm, rotation
  mid-gesture, edge→centre drift, hotplug mid-gesture, 10-finger,
  pen-during-fingers.
- **Kill switch.** `gestures { disabled true }`. Touch must never be able to
  wedge a session.
- **Minimal `input/mod.rs` footprint** — a thin call site, not 1400 lines.
  That is what keeps the fork rebasable across niri releases.

## Landing strategy

Five independently-upstreamable PRs, each testable alone:

| PR | Contents | From |
|---|---|---|
| 1 | Touchpad gesture triggers unified into the bind grammar | niri-touch |
| 2 | Config grammar for touch binds (`bind Swipe fingers=N …`) | both |
| 3 | Contact tracking (L1) — pure, headless | both + corner merge |
| 4 | Recognizer (L2) — pure, golden-trace tested | both |
| 5 | Arbitration + window rules + exclusion (L3) | niri-touch passthrough + Android rects |

niri-tablet contributes PR 3's corner zones and discrete-flick tier logic,
PR 4's `horizontal-swipe "resize-column"`, and its `gesture-debug:` logging.
niri-touch contributes the gesture taxonomy (swipe/pinch/rotate/tap/edge),
the spread and rotation math, zoned edges, and the passthrough concept.
**Neither contributes its ownership model.**

## Open: check this before building further

niri-tablet's README:149-150:

> Upstream is working on [configurable gesture binds][pr3771]; when that
> lands, this patchset can be retired.

**Check [niri-wm/niri#3771][pr3771]'s status and the config grammar upstream
is converging on.** If upstream is landing parameterized binds, PRs 1–2 land
nearly as-written and this fork's bet is small. If upstream is still on a
fixed node scheme, the fork earns its keep and PR 2 becomes the upstreamable
artifact either way.

PR 3 (L1) has no dependency on that answer, which is why it goes first.

[pr3771]: https://github.com/niri-wm/niri/pull/3771

## Status

| PR | State |
|---|---|
| 3 — L1 contact tracking | **done** |
| 4 — L2 recogniser | **done** |
| 5 — L3 arbitration | not started |
| 1, 2 — config grammar | blocked on the [niri-wm/niri#3771][pr3771] check above |

`crates/touch-core` now carries both lower layers: **70 tests, clippy clean,
zero dependencies**.

L1 currently provides: slot lifecycle (`down`/`motion`/`up`/`clear`), per-frame
derived geometry (`centroid`, `centroid_delta`, `spread`, `rotation_delta`,
`total_motion`), and zone classification merging both forks' edge/corner models
with corner-takes-precedence. Geometry is lifted from niri-touch
(`calculate_spread`, `calculate_per_slot_angles`, `calculate_rotation_delta`,
`detect_edge`) and niri-tablet (`EDGE_ZONE_PX`), with the smithay and
`niri-config` types dropped — those belong to L3.

L2 provides `Recognizer`, which owns an L1 `ContactSet` and emits
`GestureEvent`s for `Swipe`, `Pinch`, `Rotate`, `Tap`, `TapHold`, `EdgeSwipe`
and `CornerSwipe`. Three properties it guarantees, each with tests:

- **Lifecycle.** Exactly one `Begin` and one `End` per gesture. `end_active`
  is the single exit from the active state, so finger lift, device removal and
  touch cancel all close a gesture rather than dropping it. This is the
  invariant niri-tablet patch `0011` had to learn the hard way.
- **Arbitration by relative overshoot.** Swipe, pinch and rotation compete on
  `metric / threshold`; the furthest past wins. No priority table.
- **Direction and axis lock.** A recognised gesture's kind, direction and axis
  are fixed until it ends.

One non-obvious rule earned its way in via a failing test: **rotation is only
admissible when the cluster is not travelling** (`travel < spread`). Collinear
contacts — the ordinary shape of a vertical multi-finger swipe — have
ill-conditioned per-slot angles, and a plain downward drag produced a phantom
rotation of ≈π/6, over 3× the rotate threshold and enough to beat a real
swipe. A rigid rotation holds the centroid still; a translation does not.

## Layout

```
niri-toublet/
├── DESIGN.md              # this file
└── crates/
    └── touch-core/        # L1 + L2, zero dependencies
        ├── src/
        │   ├── lib.rs
        │   ├── contact.rs # L1: slot lifecycle + derived geometry + zones
        │   └── gesture.rs  # L2: recogniser
        └── tests/
            ├── golden_trace.rs # L1, driven headlessly
            └── recognizer.rs   # L2 behaviour + lifecycle invariant
```
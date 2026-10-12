# NIRI_TOUCH_CORE

The **L1/L2/L3 touchscreen rewrite** for the [niri](https://github.com/niri-wm/niri)
Wayland compositor, held as its own top-level project.

This is a clean-room third implementation, built after studying two prior
touchscreen efforts: `niri-touch` (a whole-tree fork) and the `niri-tablet`
patch series. `DESIGN.md` in `niri-toublet/` has the full design rationale;
this README is the orientation map.

---

## Layout — two repos

```
NIRI_TOUCH_CORE/
├── niri/           the niri fork carrying the rewrite
│                   branch: touch-core-l1l2l3   tip: e774e9ae
│                   remote: origin → https://github.com/niri-wm/niri.git
└── niri-toublet/   the pure L1/L2/L3 crates
                    branch: master   tip: 9890ab7   (7 commits)
                    remote: NONE  ← see the warning at the bottom
```

**`niri/`** is a fork of niri at upstream commit `2c82b77a` with the
touchscreen rewrite applied on top as a single commit, `e774e9ae`
("Touch: L1/L2/L3 touchscreen rewrite"). That commit changes **22 files,
+7475 −109**. The bulk of it is `src/input/touch_arbitration_grab.rs`
(2,667 lines) and `src/tests/touch.rs` (832 lines), plus edits to
`src/input/mod.rs`, `niri-config/src/*`, `niri-ipc/src/lib.rs`, `src/niri.rs`,
`src/ipc/*`, `src/ui/*`, `src/window/mod.rs`, and
`docs/wiki/Configuration:-Input.md`.

It depends on `niri-toublet` through two `path` dependencies declared in
`niri/Cargo.toml`:

```toml
touch-arbitrate = { path = "../niri-toublet/crates/touch-arbitrate" }
touch-core      = { path = "../niri-toublet/crates/touch-core" }
```

**`niri-toublet/`** is a small Cargo workspace holding the pure crates:
`crates/touch-core` (L1 + L2) and `crates/touch-arbitrate` (L3).

> ⚠️ **The `../` in those paths is load-bearing.** `niri-toublet/` must be a
> **sibling** of `niri/`, directly inside this project root. If you move one
> without the other, the relative path breaks and the build fails. Do not fix
> this by editing `Cargo.toml` — fix the directory layout.

---

## The three layers

The stack is split into three layers with **one-way dependencies**, so that
each can be tested in isolation and the layers above stay small.

```
┌─ L3  ARBITRATION ─────────────────────────────────┐
│  Who owns this touch?  window rules, exclusion rects │
├─ L2  RECOGNITION ─────────────────────────────────┤
│  Pure state machines.  ContactFrame* → GestureEvent │
├─ L1  CONTACT ─────────────────────────────────────┐
│  Slots · centroid · spread · angle · edge zone       │
│  No policy.                                          │
└─────────────────────────────────────────────────────┘
```

### L1 — contact (`touch-core`)

L1 tracks the live set of touch contacts and, for each frame, derives the
geometry a recogniser needs: the centroid and its frame-to-frame delta, the
spread (mean distance to centroid), the signed rotation delta, and total
motion. It contains **no policy** — it decides nothing about what a gesture
*means*, and applies no thresholds. Contacts are keyed by a `Slot` id, which
is what makes the two failure modes that broke both prior forks — late-joining
fingers and device removal — handled by construction rather than patched over
later. Implemented in `crates/touch-core/src/contact.rs` (1,156 lines).

### L2 — recognition (`touch-core`)

L2 consumes the per-frame geometry from L1 and emits a stream of
`GestureEvent`s. It is pure: no compositor, no Wayland, no clock, no config
parser, no actions. A recognised gesture has a strict lifecycle —
`Begin → Update* → End { completed }` — with exactly one `Begin` and exactly
one `End`. Every exit path (finger lift, device removal, touch cancel) routes
through `Recognizer::end_active`, which is the only way an active gesture is
cleared, so a `Begin` can never go unanswered. Implemented in
`crates/touch-core/src/gesture.rs` (770 lines).

Because L2 is headless, you can feed it recorded contact traces and assert on
gesture output — the gesture suite runs in milliseconds instead of requiring
a compositor build.

### L3 — arbitration (`touch-arbitrate` + compositor adapter)

L3 answers exactly one question per touch-down: **who owns this touch?** —
the compositor or the client. It is a pure function of the down-context and
the config: window rules, exclusion rects, modifier state, pen-vs-finger, and
device identity decide ownership. Two details worth knowing: ownership is not
a per-contact constant, because fingers accumulate on a plain surface and the
Nth one (3 by default, `Policy::min_gesture_fingers`) arms the gesture for the
whole sequence; and the arbitrator is stateless with respect to the recogniser.

The pure half lives in `crates/touch-arbitrate/src/lib.rs` (1,285 lines). The
compositor half is `niri/src/input/touch_arbitration_grab.rs`, which is
deliberately thin: it translates niri's "where did this finger land" into a
`DownContext`, installs and uninstalls the smithay grab at the right moments,
and routes each event to either the recogniser or the client. Keeping that
file small is the entire point — the two forks this replaces both grew past a
thousand lines by fusing tracker, recogniser and seat bookkeeping into one
`enum State`, and the bugs needing 20 patches to paper over were almost all
bugs in that fused state machine.

---

## Build and test

Both repos are ordinary Cargo projects. Build **from `niri/`**:

```sh
cd niri
cargo check --lib          # 0 errors
cargo test --workspace     # 228 passed, 0 failed (plus niri-config/niri-ipc suites)
```

The pure crates test standalone in milliseconds, with no compositor build:

```sh
cd niri-toublet
cargo test                 # 120 passed, 0 failed
```

If you are short on disk, point cargo at a target dir on the real volume
rather than `/tmp`, which is a small tmpfs here:

```sh
CARGO_TARGET_DIR=/path/on/real/disk/.target-check cargo test --workspace
```

---

## Relationship to `../NIRI/`

**The two projects are independent. Neither depends on the other.**

`../NIRI/` is the older **niri-tablet** work: a 37-patch series in
`../NIRI/patches/series/`, applied by `../NIRI/apply-patches.sh` onto
`../NIRI/niri-patched/` for packaging via `../NIRI/pkg/PKGBUILD`.

| | `NIRI_TOUCH_CORE/` (this) | `../NIRI/` (niri-tablet) |
|---|---|---|
| Shape | a fork + separate pure crates | a patch series vs upstream |
| Touch layers | L1/L2/L3, three-way split | fused into `src/input/mod.rs` |
| Config | one `bind` grammar with `fingers=` properties | fixed `gestures {}` nodes |

`DESIGN.md` argues at length that niri-tablet's touch handling is a
different, earlier design and that this rewrite deliberately discards its
ownership model. Concretely:

- This project does **not** read or apply `../NIRI/patches/`.
- The patch series does **not** apply to this repo's `niri/` — it targets
  upstream niri tags, whereas this `niri/` carries a large unmerged rewrite.
- `apply-patches.sh` uses `../NIRI/niri` (pristine upstream) as its input and
  writes to `../NIRI/niri-patched/`. It never touches this directory.

Because of that last point, **`../NIRI/niri/` is retained**, still on branch
`touch-core-l1l2l3` at `e774e9ae`, as the pristine input for the patch
series. `../NIRI/niri-toublet/` is likewise retained: the `touch-core-l1l2l3`
checkout in `../NIRI/niri` still resolves its `../niri-toublet/crates/*`
path dependencies from there. Removing either would break that checkout, so
this project holds verified *copies* rather than having moved the originals.

---

## ⚠️ CRITICAL WARNING — `niri-toublet` has no remote

`niri-toublet/` is a git repository with **7 commits and no configured
remote**. It has never been pushed anywhere. `niri/` is safe by comparison:
its only remote is public upstream `github.com/niri-wm/niri.git`, and the
rewrite commit `e774e9ae` exists on a local branch there, not upstream.

**If this disk fails, the pure crates are gone.** There is no second copy.
Give `niri-toublet/` a remote and push it, or take a backup, before relying on
this project.
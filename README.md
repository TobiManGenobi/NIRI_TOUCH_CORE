# NIRI_TOUCH_CORE

The **L1/L2/L3 touchscreen rewrite** for the [niri](https://github.com/niri-wm/niri)
Wayland compositor, held as its own top-level project.

This is a clean-room third implementation, built after studying two prior
touchscreen efforts: `niri-touch` (a whole-tree fork) and the `niri-tablet`
patch series. `DESIGN.md` at the repository root has the full design rationale;
this README is the orientation map.

---

## Layout — one repository, one cargo workspace

`NIRI_TOUCH_CORE/` is a **single git repository** *and* a **single cargo
workspace**. The compositor fork and the pure touch crates are ordinary
directories inside it:

```
NIRI_TOUCH_CORE/          ← the one repository (branch: main)
├── Cargo.toml            ← the one workspace root (package: niri)
├── src/                  the compositor source
├── crates/
│   ├── touch-core/       L1 + L2, zero dependencies
│   └── touch-arbitrate/  L3, one dependency: touch-core
├── niri-config/  niri-ipc/  niri-visual-tests/
├── resources/  docs/  build.rs  flake.nix  niri.spec.rpkg  …
├── DESIGN.md             the architecture rationale
├── README.niri.md        upstream niri's own README (unchanged)
└── README.md             this file
```

The two histories were originally separate projects, combined with
`git subtree add --prefix=…`, and are both real commits in this one
repository, still reachable by their original hashes (`e774e9ae`, `9890ab7`).
There are **no nested `.git` directories** and no submodules. The upstream niri
ancestry is intact, so the niri side can still be rebased onto future
upstream releases.

The niri fork descends from upstream commit `2c82b77a` with the touchscreen
rewrite applied on top as a single commit, `e774e9ae` ("Touch: L1/L2/L3
touchscreen rewrite"). That commit changes **22 files, +7475 −109**. The bulk
of it is `src/input/touch_arbitration_grab.rs` (2,667 lines) and
`src/tests/touch.rs` (832 lines), plus edits to `src/input/mod.rs`,
`niri-config/src/*`, `niri-ipc/src/lib.rs`, `src/niri.rs`, `src/ipc/*`,
`src/ui/*`, `src/window/mod.rs`, and `docs/wiki/Configuration:-Input.md`.

The crates are consumed from the root `Cargo.toml` by in-tree path:

```toml
touch-arbitrate = { path = "crates/touch-arbitrate" }
touch-core      = { path = "crates/touch-core" }
```

They are also listed in `[workspace] members`, so they build, test and lint
as part of `cargo --workspace`, and they are covered by the repository's CI.

> **History.** This tree used to be two directories — `niri/` and
> `niri-toublet/` — with the crates reached through `../niri-toublet/…`. That
> made the sibling relationship load-bearing: moving one directory without the
>> other broke the build. Flattening removed the constraint rather than
> documenting it.

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
compositor half is `src/input/touch_arbitration_grab.rs`, which is
deliberately thin: it translates niri's "where did this finger land" into a
`DownContext`, installs and uninstalls the smithay grab at the right moments,
and routes each event to either the recogniser or the client. Keeping that
file small is the entire point — the two forks this replaces both grew past a
thousand lines by fusing tracker, recogniser and seat bookkeeping into one
`enum State`, and the bugs needing 20 patches to paper over were almost all
bugs in that fused state machine.

---

## Build and test

Everything is one cargo project, rooted at this directory. Build **from the
repository root**:

```sh
cd NIRI_TOUCH_CORE
cargo check --lib          # 0 errors
cargo test --workspace     # every suite, including the pure touch crates
```

The pure crates are workspace members, so their ~120 tests run as part of
`--workspace` — there is no second project to remember. To iterate on just
them, without a compositor build:

```sh
cargo test -p touch-core -p touch-arbitrate   # 120 passed, 0 failed
```

**Always set `CARGO_TARGET_DIR` to somewhere on the real disk.** `/tmp` is a
small tmpfs (3.8G) and a niri build needs roughly 10G, so a default target
dir under `/tmp` dies with `Disk quota exceeded`:

```sh
CARGO_TARGET_DIR=/run/media/tobiasmagnegraneberg/Developer/Developer/.target-check \
  cargo test --workspace
```

The root `.gitignore` excludes `/target/`, so build artifacts are never
tracked.

---

## Relationship to `../NIRI/`

**The two projects are independent. Neither depends on the other.**

`../NIRI/` is the older **niri-tablet** work: a 37-patch series in
`../NIRI/patches/series/`, applied by `../NIRI/apply-patches.sh` onto
`../NIRI/niri-patched/` for packaging via `../NIRI/pkg/PKGBUILD`.

| | `NIRI_TOUCH_CORE/` (this) | `../NIRI/` (niri-tablet) |
|---|---|---|
| Shape | one repo: a fork + pure crates as subdirs | a patch series vs upstream |
| Touch layers | L1/L2/L3, three-way split | fused into `src/input/mod.rs` |
| Config | one `bind` grammar with `fingers=` properties | fixed `gestures {}` nodes |

`DESIGN.md` argues at length that niri-tablet's touch handling is a
different, earlier design and that this rewrite deliberately discards its
ownership model. Concretely:

- This project does **not** read or apply `../NIRI/patches/`.
- The patch series does **not** apply to this repository — it targets
  upstream niri tags, whereas this tree carries a large unmerged rewrite.
- `apply-patches.sh` uses `../NIRI/niri` (pristine upstream) as its input and
  writes to `../NIRI/niri-patched/`. It never touches this directory.

Because of that last point, **`../NIRI/niri/` is retained**, still on branch
`touch-core-l1l2l3` at `e774e9ae`, as the pristine input for the patch
series. `../NIRI/niri-toublet/` is likewise retained: that checkout still
resolves its `../niri-toublet/crates/*` path dependencies from there, and that
layout is unchanged. Removing either would break it, so it keeps its own
copies. This project is unaffected: it builds entirely from the directories
inside `NIRI_TOUCH_CORE/`, and never reads anything under `../NIRI/`.

---

## ⚠️ WARNING — this repository has no remote

This repository has **no configured remote and has never been pushed
anywhere**. The `niri-toublet` history that used to live in its own
un-backed-up repository now lives here, in this root repository, as real
reachable commits (tip `9890ab7`). The rewrite commit `e774e9ae` likewise
lives here, with the upstream niri ancestry behind it.

That resolves the old "the pure crates exist in exactly one place" hazard in
the sense that both histories are now in a single git repository — but the
repository itself is still only on **this disk**. Unifying the repos did not
create a backup.

**So the risk is still present, just relocated.** Before relying on this
project, either give this repository a remote and push it, or take a backup
of `NIRI_TOUCH_CORE/.git`. Upstream `github.com/niri-wm/niri.git` holds the
niri history but neither the rewrite commit nor the `niri-toublet` commits,
so cloning upstream does not recover this work.
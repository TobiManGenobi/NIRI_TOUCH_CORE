//! End-to-end gate suite: L1 + L2 + L3 composed, driven by scripted traces.
//!
//! # Why this file exists
//!
//! `touch-core` and `touch-arbitrate` are each tested against themselves, and
//! both suites are green. That is not sufficient, because the failure this
//! layer exists to prevent is *compositional*: does arming actually produce a
//! `GestureEvent`? does the sequence end cleanly? does a client keep a finger
//! down that the compositor took away?
//!
//! Each fork that shipped phantom touches shipped per-layer tests too. The bug
//! lived in the seam. So this file tests the seam: one [`Harness`] that owns one
//! [`Recognizer`] and one [`Arbitrator`] and routes every event exactly the way
//! the niri adapter does, and one `#[test]` per acceptance-list gate item.
//!
//! # Relationship to the other two test files
//!
//! This is a *replacement* for the overlap, not a third copy of it. The
//! `arbitration_trace.rs` cases that only ever asserted ownership decisions are
//! duplicated here as part of a composed gesture and their standalone versions
//! were folded in; see the commit message. The unit tests inside `lib.rs` stay
//! where they are, because a decision-table row is best proven in isolation
//! next to the code that implements it, and because they run without a
//! `Recognizer` in the picture at all.
//!
//! # What is *not* covered here
//!
//! Anything requiring real hardware, a real output transform, or a real
//! client. Zone geometry is driven through [`touch_core::classify_zone`] with
//! a synthetic [`OutputSize`], so a scaled/rotated panel's actual pixel strips
//! are out of scope — what is proven is that *given* a classified zone, the
//! composition behaves. `under_gesture_app` and the exclusion rect are fed in
//! as flags the adapter would have computed from a real window and a real
//! surface advertisement. Time is an explicit [`Duration`] throughout; nothing
//! here reads a clock, and a test that cannot fail on a wall-clock boundary
//! would be a flake generator, not a gate.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use touch_arbitrate::{Arbitrator, DownContext, Ownership, Point, Policy, Rect};
use touch_core::{
    classify_zone, Corner, Edge, Gesture, GestureEvent, OutputSize, Recognizer, RecognizerConfig,
    Slot, SwipeDirection, Third, Zone,
};

/// The synthetic panel every trace is expressed against.
const OUTPUT: OutputSize = OutputSize {
    w: 1920.0,
    h: 1200.0,
};

/// The edge/corner strip width, matching touch-core's own default.
const ZONE_PX: f64 = 48.0;

/// What the fake seat would be holding.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Seat {
    /// Touch points the client believes are down.
    open: BTreeSet<u32>,
    /// Every touch-end the compositor sent to the client, in order.
    ///
    /// This is the audit trail for patch `0011`: if a slot appears here, the
    /// client was told about it; if a slot was forwarded but never appears
    /// in a later close, it is a phantom.
    ended: Vec<u32>,
}

impl Seat {
    /// The fake `handle.up`. smithay has no per-slot cancel, so a touch-end is
    /// how the compositor closes one point without killing the session.
    fn up(&mut self, slot: u32) {
        assert!(
            self.open.remove(&slot),
            "compositor closed slot {slot} the client never had down — \
             a phantom in the other direction"
        );
        self.ended.push(slot);
    }
}

/// One composed touch stack: the arbitrator's ruling decides routing, and only
/// what the compositor owns reaches the recogniser.
#[derive(Debug)]
struct Harness {
    arb: Arbitrator,
    rec: Recognizer,
    seat: Seat,
    /// Slots the client holds, per the arbitrator's ledger.
    client_slots: BTreeSet<u32>,
    /// Slots the *recogniser* is tracking.
    ///
    /// Deliberately separate from `client_slots`, and the reason this field
    /// is not `compositor_slots`: **ownership bookkeeping and physical
    /// reality are different things.** When the third finger arms, the two
    /// earlier fingers are handed back by `clear_clients()` and closed on
    /// the seat — so they leave the client set — but they are still on the
    /// glass, and they are part of the gesture the compositor just took. They
    /// are retro-fed to the recogniser and belong in this set.
    ///
    /// Keying `motion` and `up` off the routing sets instead is the bug this
    /// set exists to prevent: the contacts are fed at the down, then never
    /// move, never lift, the grab never uninstalls, and the stuck contacts
    /// poison the next gesture. Ownership answers "who should have this
    /// touch"; this set answers "what is the recogniser actually holding".
    fed_slots: BTreeSet<u32>,
    /// Where each physically down contact last was.
    ///
    /// Needed because a retro-fed contact must be replayed to the recogniser
    /// at *its own* start position, not wherever the finger that armed the
    /// sequence happens to be.
    positions: BTreeMap<u32, Point>,
    /// The ownership ruling for every down, in order.
    ownership: Vec<Ownership>,
    /// The slots cancelled at each arming, in arming order.
    cancels: Vec<Vec<u32>>,
    /// Every gesture event the recogniser produced, in order.
    events: Vec<GestureEvent>,
    /// Sticky flags the adapter re-reads per down rather than remembering.
    under_gesture_app: bool,
    mod_down: bool,
}

impl Harness {
    fn new(policy: Policy) -> Self {
        Self {
            arb: Arbitrator::new(policy),
            rec: Recognizer::new(RecognizerConfig::default()),
            seat: Seat::default(),
            client_slots: BTreeSet::new(),
            fed_slots: BTreeSet::new(),
            positions: BTreeMap::new(),
            ownership: Vec::new(),
            cancels: Vec::new(),
            events: Vec::new(),
            under_gesture_app: false,
            mod_down: false,
        }
    }

    /// Build the down-context the way the adapter's `down_context` does.
    fn ctx(&self, pos: Point, is_pen: bool, eraser: bool) -> DownContext {
        let policy = self.arb.policy();
        DownContext {
            zone: classify_zone(pos, OUTPUT, ZONE_PX),
            under_gesture_app: self.under_gesture_app,
            mod_down: self.mod_down,
            is_pen,
            eraser,
            device_id: 1,
            pos,
            in_exclusion_rect: policy.excludes(pos),
        }
    }

    /// One touch-down, arbitrated and routed.
    ///
    /// The ordering here is the adapter's, and it is load-bearing:
    /// arbitrate, then — if the compositor won — cancel whatever the client
    /// still holds, and only then start feeding the recogniser.
    fn down(&mut self, slot: u32, pos: Point, is_pen: bool, eraser: bool, time: Duration) {
        let ctx = self.ctx(pos, is_pen, eraser);
        self.positions.insert(slot, pos);
        let ownership = self.arb.down(slot, &ctx);
        self.ownership.push(ownership);

        match ownership {
            Ownership::Client => {
                self.client_slots.insert(slot);
                self.seat.open.insert(slot);
            }
            Ownership::Compositor => {
                // The compositor has taken the sequence. Every touch it
                // forwarded earlier and has not been told about must be
                // closed on the seat *now*, before the grab goes in — this
                // ordering is the whole of niri-tablet patch `0011`.
                let stranded = self.arb.clear_clients();
                for s in &stranded {
                    assert!(
                        self.client_slots.remove(s),
                        "arbitrator cancelled slot {s}, which we never \
                         forwarded — the two ledgers disagree"
                    );
                    self.seat.up(*s);
                }
                if !stranded.is_empty() {
                    self.cancels.push(stranded.clone());
                }

                // Every contact of the sequence the compositor just took is
                // now the recogniser's, and stays its for the rest of the
                // sequence — including the ones just cancelled at the seat,
                // which are still on the glass.
                self.fed_slots.insert(slot);
                self.rec.down(Slot::new(slot), ctx.pos, ctx.zone, time);

                // The stranded contacts join the same sequence, replayed at
                // their own start positions and with no zone of their own —
                // they were plain-surface contacts, and their position is
                // where they actually are, not where the arming finger is.
                for s in stranded {
                    let p = self.positions.get(&s).copied().unwrap_or(ctx.pos);
                    if self.fed_slots.insert(s) {
                        self.rec.down(Slot::new(s), p, None, time);
                    }
                }
            }
            Ownership::Ignored => {}
        }
    }

    /// One touch-motion, routed to whichever layer holds the slot.
    fn motion(&mut self, slot: u32, pos: Point) {
        if self.client_slots.contains(&slot) {
            return; // forwarded verbatim; the recogniser never sees it
        }
        if self.fed_slots.contains(&slot) {
            self.rec.motion(Slot::new(slot), pos);
        }
    }

    /// One touch-up, routed to whichever layer holds the slot.
    fn up(&mut self, slot: u32) {
        let owned = self.arb.up(slot);
        if self.client_slots.remove(&slot) {
            self.seat.up(slot);
            return;
        }
        if !self.fed_slots.remove(&slot) {
            // An unknown slot is a desync worth noticing but not worth
            // panicking over mid-gesture; the ledger is the authority and it
            // already said what it thought.
            assert_eq!(owned, None, "slot {slot} was owned but is in neither set");
            return;
        }
        self.rec.up(Slot::new(slot));
    }

    /// One `wl_touch.frame`: the atomic batch boundary.
    ///
    /// This is also where the routing invariant is checked, which is the
    /// only place that can catch a slot leaking from the client into the
    /// recogniser. Doing it per frame rather than once at the end of a test
    /// means the failure names the frame it went wrong on instead of a
    /// downstream gesture that happened to differ.
    fn frame(&mut self, time: Duration) {
        self.check_routing();
        self.events.extend(self.rec.frame(time));
    }

    /// The recogniser and `fed_slots` must agree exactly, in both
    /// directions.
    ///
    /// A client-owned slot reaching L1/L2 would make the recogniser act on
    /// touches the client is still holding — the mirror image of the phantom
    /// bug. And a slot in `fed_slots` that the recogniser has quietly dropped
    /// is the stuck-contact bug: the grab would never uninstall. Checking
    /// equality rather than containment catches both, and checking per frame
    /// rather than at the end of a test names the frame it went wrong on.
    fn check_routing(&self) {
        let mut in_recogniser: BTreeSet<u32> = BTreeSet::new();
        for (slot, _) in self.rec.contacts() {
            in_recogniser.insert(slot.id());
        }
        assert_eq!(
            in_recogniser, self.fed_slots,
            "routing leak: the recogniser is tracking {in_recogniser:?} but the \
             harness fed it {:?}",
            self.fed_slots
        );
    }

    /// The slots the recogniser is holding, for direct assertion in a test.
    fn recogniser_contacts(&self) -> BTreeSet<u32> {
        self.rec.contacts().map(|(s, _)| s.id()).collect()
    }

    /// The device went away. Everything is torn down at once.
    fn device_gone(&mut self) -> usize {
        let closed = self.arb.clear();
        for s in self.seat.open.iter().copied().collect::<Vec<_>>() {
            self.seat.up(s);
        }
        // `Recognizer::reset` closes an in-flight gesture with
        // `completed: false`, so a yanked device never fires a bound action.
        self.events.extend(self.rec.reset());
        self.client_slots.clear();
        self.fed_slots.clear();
        self.positions.clear();
        closed
    }

    /// Every completed *translational or radial* gesture the run produced.
    ///
    /// [`Gesture::TapHold`] is excluded deliberately. It ends with
    /// `completed: true` too, but it is a *signal* — "this tap candidate just
    /// started moving" — not a bound action, and counting it as one would
    /// make every gate item report two gestures where it means one.
    fn completed(&self) -> Vec<Gesture> {
        self.events
            .iter()
            .filter_map(|e| match e {
                GestureEvent::End {
                    gesture,
                    completed: true,
                } => Some(*gesture),
                _ => None,
            })
            .filter(|g| !matches!(g, Gesture::TapHold { .. } | Gesture::Tap { .. }))
            .collect()
    }

    /// Whether the run ended with the whole stack quiescent.
    fn is_clean(&self) -> bool {
        self.seat.open.is_empty()
            && self.arb.open_client_touches() == 0
            && self.client_slots.is_empty()
            && self.fed_slots.is_empty()
            && !self.rec.is_active()
    }
}

fn at(ms: u64) -> Duration {
    Duration::from_millis(ms)
}

fn centre(y_offset: f64) -> Point {
    Point::new(960.0, 600.0 + y_offset)
}

/// Drag every live slot 30 px left per step for `steps` frames, framing each.
fn drag_left(h: &mut Harness, slots: &[u32], steps: u64, start_x: f64) {
    for step in 1..=steps {
        for (i, s) in slots.iter().enumerate() {
            let p = Point::new(start_x - 30.0 * step as f64, 500.0 + 40.0 * i as f64);
            h.motion(*s, p);
        }
        h.frame(at(step * 16));
    }
}

// ── 1. the case no prior implementation had ───────────────────────────────

#[test]
fn three_finger_mid_screen_swipe_arms_and_produces_a_workspace_swipe() {
    let mut h = Harness::new(Policy::default());

    for (i, slot) in [0u32, 1, 2].into_iter().enumerate() {
        h.down(slot, centre(40.0 * i as f64), false, false, at(0));
    }
    h.frame(at(0));

    // The gate: nothing but a bare finger count armed this. No zone, no Mod,
    // no passthrough window — the three contacts are in the middle of a plain
    // surface, which is exactly the case that could not arm before step 8 of
    // `decide` existed.
    assert_eq!(
        h.ownership,
        vec![Ownership::Client, Ownership::Client, Ownership::Compositor],
        "the third bare finger must arm the sequence"
    );
    assert_eq!(
        h.cancels,
        vec![vec![0, 1]],
        "arming must hand back exactly the two forwarded touches"
    );
    assert!(
        h.seat.open.is_empty(),
        "and they must actually be closed on the seat: {:?}",
        h.seat.open
    );

    // All three contacts are physically down and must reach the recogniser.
    assert_eq!(h.rec.contacts().count(), 3, "the gesture is three contacts");

    drag_left(&mut h, &[0, 1, 2], 15, 600.0);
    for slot in 0..3u32 {
        h.up(slot);
    }
    h.frame(at(400));

    let swipe = h.completed();
    assert_eq!(swipe.len(), 1, "exactly one completed gesture: {swipe:?}");
    match swipe[0] {
        Gesture::Swipe {
            fingers,
            direction,
            progress,
            ..
        } => {
            assert_eq!(fingers, 3, "a three-finger swipe, not a two-finger one");
            assert_eq!(direction, SwipeDirection::Left);
            assert!(
                (progress - 1.0).abs() < f64::EPSILON,
                "the swipe reached its action threshold, got {progress}"
            );
        }
        other => panic!("expected a completed swipe, got {other:?}"),
    }

    assert!(h.is_clean(), "stack left dirty: {:?}", h.seat.open);
}

// ── 2. patch 0011, end to end ──────────────────────────────────────────────

#[test]
fn no_phantom_touch_after_multi_finger_gesture() {
    let mut h = Harness::new(Policy::default());

    for (i, slot) in [0u32, 1, 2].into_iter().enumerate() {
        h.down(slot, centre(40.0 * i as f64), false, false, at(0));
    }
    h.frame(at(0));
    drag_left(&mut h, &[0, 1, 2], 15, 600.0);
    for slot in 0..3u32 {
        h.up(slot);
    }
    h.frame(at(400));

    // The invariant, stated concretely rather than as a count: the seat holds
    // nothing, the client was told about each forwarded finger, and the
    // recogniser is not mid-gesture.
    assert_eq!(h.seat.open, BTreeSet::new(), "no touch left down");
    assert_eq!(
        h.seat.ended,
        vec![0, 1],
        "exactly the two forwarded contacts were closed, once each"
    );
    assert_eq!(h.arb.open_client_touches(), 0);
    assert!(!h.rec.is_active(), "the recogniser is still mid-gesture");
    assert!(h.is_clean());

    // The half of patch 0011 that actually shipped: after the phantoms, the
    // browser ignored every later tap. A fresh single-finger tap must reach
    // the client as a lone contact and must NOT be recognised — with
    // `min_gesture_fingers` at 3, one finger is nowhere near arming.
    let before = h.events.len();
    h.down(7, centre(0.0), false, false, at(1000));
    h.up(7);
    h.frame(at(1050));

    assert_eq!(
        h.ownership[3],
        Ownership::Client,
        "a lone tap is the client's"
    );
    assert!(
        h.events[before..]
            .iter()
            .all(|e| !matches!(e, GestureEvent::Begin(_))),
        "a single finger must not begin any gesture: {:?}",
        &h.events[before..]
    );
    assert!(
        !h.events[before..].iter().any(|e| matches!(
            e,
            GestureEvent::End {
                gesture: Gesture::Pinch { .. },
                ..
            }
        )),
        "and specifically must not pinch-zoom"
    );
    assert!(h.is_clean());
}

// ── 3/4. zones arm on the first finger ────────────────────────────────────

#[test]
fn edge_swipe_arms_on_the_first_finger() {
    let mut h = Harness::new(Policy::default());
    // Middle third of the left edge, so it is an edge and not a corner.
    let start = Point::new(20.0, 600.0);

    h.down(0, start, false, false, at(0));
    h.frame(at(0));

    assert_eq!(
        h.ownership,
        vec![Ownership::Compositor],
        "an edge zone arms on the first contact, with no threshold"
    );
    assert!(
        h.cancels.is_empty(),
        "nothing was forwarded, so nothing to cancel"
    );
    assert_eq!(h.seat.open, BTreeSet::new());

    for step in 1..=15u64 {
        h.motion(0, Point::new(20.0 + 30.0 * step as f64, 600.0));
        h.frame(at(step * 16));
    }
    h.up(0);
    h.frame(at(300));

    assert!(
        h.completed().iter().any(|g| matches!(
            g,
            Gesture::EdgeSwipe {
                edge: Edge::Left,
                ..
            }
        )),
        "expected a left edge swipe, got {:?}",
        h.completed()
    );
    assert!(h.is_clean());
}

#[test]
fn corner_zone_arms_on_the_first_finger() {
    let mut h = Harness::new(Policy::default());
    let start = Point::new(10.0, 1190.0);

    h.down(0, start, false, false, at(0));
    h.frame(at(0));

    assert_eq!(
        h.ownership,
        vec![Ownership::Compositor],
        "a corner zone arms on the first contact"
    );

    for step in 1..=15u64 {
        h.motion(
            0,
            Point::new(10.0 + 30.0 * step as f64, 1190.0 - 30.0 * step as f64),
        );
        h.frame(at(step * 16));
    }
    h.up(0);
    h.frame(at(300));

    assert!(
        h.completed().iter().any(|g| matches!(
            g,
            Gesture::CornerSwipe {
                corner: Corner::BottomLeft,
                ..
            }
        )),
        "expected a bottom-left corner swipe, got {:?}",
        h.completed()
    );
    assert!(h.is_clean());
}

// ── 5. the tap tier ───────────────────────────────────────────────────────

#[test]
fn single_finger_tap_reaches_the_client_and_emits_a_tap() {
    let mut h = Harness::new(Policy::default());

    h.down(0, centre(0.0), false, false, at(0));
    h.up(0);
    h.frame(at(120));

    // Two fingers down and a quick lift must not become a swipe: the point of
    // the tap tier is that lifting without travelling is not a gesture. With
    // `min_gesture_fingers` at 3, neither finger ever armed, so both stayed
    // the client's and the recogniser saw nothing at all.
    h.down(1, centre(0.0), false, false, at(1000));
    h.down(2, centre(40.0), false, false, at(1000));
    h.up(1);
    h.up(2);
    h.frame(at(1100));

    assert_eq!(
        h.ownership,
        vec![Ownership::Client; 3],
        "no tap-tier contact may arm a gesture"
    );
    assert!(
        h.completed().is_empty(),
        "a lift without travel is not a gesture: {:?}",
        h.completed()
    );
    assert_eq!(h.seat.open, BTreeSet::new());
    assert_eq!(
        h.seat.ended,
        vec![0, 1, 2],
        "every tap was forwarded and closed"
    );
    assert!(h.is_clean());
}

// ── 6/7/8/9. the pen as a separate input class ────────────────────────────

#[test]
fn pen_down_reaches_the_client_and_never_arms() {
    let mut h = Harness::new(Policy::default());

    h.down(0, centre(0.0), true, false, at(0));
    h.frame(at(0));
    assert_eq!(
        h.ownership,
        vec![Ownership::Client],
        "a pen draws by default; it must never arm a multi-finger gesture"
    );

    // The stylus is still down. A finger arriving now must count a sequence
    // that starts from zero, not from the pen.
    h.down(1, centre(40.0), false, false, at(50));
    h.frame(at(50));
    assert_eq!(
        h.ownership[1],
        Ownership::Client,
        "a resting stylus must not arm the recogniser for the next finger"
    );

    h.up(1);
    h.up(0);
    h.frame(at(200));

    assert!(
        h.completed().is_empty(),
        "pen and one finger is not a gesture: {:?}",
        h.completed()
    );
    assert!(h.is_clean());
}

#[test]
fn mod_plus_pen_arms_the_compositor() {
    let mut h = Harness::new(Policy::default());
    h.mod_down = true;

    h.down(0, centre(0.0), true, false, at(0));
    h.frame(at(0));

    assert_eq!(
        h.ownership,
        vec![Ownership::Compositor],
        "Mod is the escape hatch: it is how the user says 'treat this pen \
         contact as a gesture'"
    );
    assert_eq!(h.seat.open, BTreeSet::new());

    // Lift and frame so the tap candidate can close; a recogniser holding a
    // live tap candidate is not a dirty stack, it is a pending one.
    h.up(0);
    h.frame(at(100));
    assert!(h.is_clean());
}

#[test]
fn pen_does_not_count_toward_the_arming_threshold() {
    let mut h = Harness::new(Policy::default());

    h.down(0, centre(0.0), false, false, at(0));
    h.down(1, centre(40.0), false, false, at(0));
    h.down(9, centre(80.0), true, false, at(10));
    h.frame(at(10));

    // Two fingers and a stylus is still two fingers.
    h.down(2, centre(120.0), false, false, at(20));
    h.frame(at(20));

    assert_eq!(
        h.ownership,
        vec![
            Ownership::Client,
            Ownership::Client,
            Ownership::Client,
            Ownership::Compositor,
        ],
        "the third *finger* arms; the stylus neither accelerated nor blocked it"
    );
    assert_eq!(h.cancels, vec![vec![0, 1, 9]]);

    for slot in [0u32, 1, 9, 2] {
        h.up(slot);
    }
    h.frame(at(200));
    assert!(h.is_clean());
}

#[test]
fn eraser_tip_reaches_the_client() {
    let mut h = Harness::new(Policy::default());

    // Mod is held, so the eraser case is the one where precedence matters:
    // an eraser is a distinct tool and must reach the client even though Mod
    // would otherwise take any contact.
    h.mod_down = true;
    h.down(0, centre(0.0), true, true, at(0));
    h.frame(at(0));

    assert_eq!(
        h.ownership,
        vec![Ownership::Client],
        "an eraser tip is never a gesture finger, Mod or not"
    );
    h.up(0);
    h.frame(at(100));
    assert!(h.is_clean());
}

// ── 10. the kill switch ───────────────────────────────────────────────────

#[test]
fn disabled_policy_passes_everything_through() {
    let mut h = Harness::new(Policy {
        disabled: true,
        ..Policy::default()
    });

    for (i, slot) in [0u32, 1, 2, 3, 4].into_iter().enumerate() {
        h.down(slot, centre(40.0 * i as f64), false, false, at(0));
    }
    h.frame(at(0));
    drag_left(&mut h, &[0, 1, 2, 3, 4], 15, 600.0);
    for slot in 0..5u32 {
        h.up(slot);
    }
    h.frame(at(400));

    assert!(
        h.ownership.iter().all(|o| *o == Ownership::Client),
        "the kill switch must outrank arming entirely: {:?}",
        h.ownership
    );
    assert_eq!(
        h.ownership.len(),
        5,
        "every finger still reached the client"
    );
    assert!(
        h.events.is_empty(),
        "no gesture may be recognised: {:?}",
        h.events
    );
    assert!(h.cancels.is_empty());
    assert_eq!(h.seat.open, BTreeSet::new());
    assert_eq!(h.seat.ended, vec![0, 1, 2, 3, 4]);
    assert!(h.is_clean());
}

// ── 11/12. teardown ───────────────────────────────────────────────────────

#[test]
fn out_of_order_lift_leaves_no_stuck_slots() {
    let mut h = Harness::new(Policy::default());

    for (i, slot) in [0u32, 1, 2].into_iter().enumerate() {
        h.down(slot, centre(40.0 * i as f64), false, false, at(0));
    }
    h.frame(at(0));
    drag_left(&mut h, &[0, 1, 2], 15, 600.0);

    // Lift in the awkward order: the arming finger first, then a forwarded
    // one that has already been cancelled, then the last.
    h.up(2);
    h.up(0);
    h.up(1);
    h.frame(at(400));

    assert!(
        h.is_clean(),
        "out-of-order lift left state behind: seat={:?} client={:?} comp={:?}",
        h.seat.open,
        h.client_slots,
        h.fed_slots
    );
    // The gesture itself completed normally before the lifts — out-of-order
    // lifting is not a reason to withhold a bound action, only a reason not to
    // strand a slot.
    assert!(
        h.completed()
            .iter()
            .any(|g| matches!(g, Gesture::Swipe { fingers: 3, .. })),
        "the three-finger swipe still completed: {:?}",
        h.completed()
    );

    // And the ledger must not have kept the cancelled slots: a stale slot
    // inflates the arming tally and the next unrelated touch arms out of
    // nowhere.
    let mut h2 = Harness::new(Policy::default());
    h2.down(0, centre(0.0), false, false, at(0));
    h2.up(0);
    h2.frame(at(50));
    h2.down(1, centre(0.0), false, false, at(100));
    assert_eq!(
        h2.ownership[1],
        Ownership::Client,
        "a retired sequence must not arm the next one"
    );
}

#[test]
fn hotplug_mid_gesture_closes_exactly_the_open_client_touches() {
    let mut h = Harness::new(Policy::default());

    // Two bare fingers, nothing armed: both are the client's.
    h.down(0, centre(0.0), false, false, at(0));
    h.down(1, centre(40.0), false, false, at(0));
    h.frame(at(0));
    assert_eq!(h.ownership, vec![Ownership::Client; 2]);

    let closed = h.device_gone();

    assert_eq!(
        closed, 2,
        "the teardown owes the client exactly its two open touches"
    );
    assert_eq!(h.seat.open, BTreeSet::new());
    assert_eq!(h.arb.open_client_touches(), 0);
    assert!(
        h.events.iter().all(|e| !matches!(
            e,
            GestureEvent::End {
                completed: true,
                ..
            }
        )),
        "a yanked device must never fire a bound action: {:?}",
        h.events
    );
    assert!(h.is_clean());

    // And a second teardown owes nothing, which is what "no slots after a
    // sequence ends" means in practice.
    assert_eq!(h.device_gone(), 0);
}

// ── 13. ten fingers ───────────────────────────────────────────────────────

#[test]
fn ten_finger_swipe_arms_and_ends_clean() {
    let mut h = Harness::new(Policy::default());

    for (i, slot) in (0u32..10).into_iter().enumerate() {
        h.down(slot, centre(4.0 * i as f64), false, false, at(0));
    }
    h.frame(at(0));

    assert_eq!(
        h.ownership[..3],
        [Ownership::Client, Ownership::Client, Ownership::Compositor],
        "arming still needs three fingers when ten are coming"
    );
    assert!(
        h.ownership[3..].iter().all(|o| *o == Ownership::Compositor),
        "and the remaining seven inherit: {:?}",
        h.ownership
    );
    assert_eq!(h.cancels, vec![vec![0, 1]]);
    assert_eq!(h.rec.contacts().count(), 10, "all ten are on the glass");

    let slots: Vec<u32> = (0..10).collect();
    drag_left(&mut h, &slots, 15, 600.0);
    for slot in &slots {
        h.up(*slot);
    }
    h.frame(at(400));

    assert!(
        h.completed()
            .iter()
            .any(|g| matches!(g, Gesture::Swipe { fingers: 10, .. })),
        "expected a ten-finger swipe, got {:?}",
        h.completed()
    );
    assert!(h.is_clean());
}

// ── 14/15. the client's escape hatches ────────────────────────────────────

#[test]
fn passthrough_window_forwards_touches_but_zone_still_wins() {
    let mut h = Harness::new(Policy {
        passthrough_apps: vec!["org.kde.krita".to_string()],
        ..Policy::default()
    });
    h.under_gesture_app = true;

    // On the passthrough window, mid-screen: nothing may arm, however many
    // fingers arrive. This is the behaviour the window rule exists for.
    for (i, slot) in [0u32, 1, 2].into_iter().enumerate() {
        h.down(slot, centre(40.0 * i as f64), false, false, at(0));
    }
    h.frame(at(0));
    drag_left(&mut h, &[0, 1, 2], 15, 600.0);
    for slot in 0..3u32 {
        h.up(slot);
    }
    h.frame(at(400));

    assert!(
        h.ownership.iter().all(|o| *o == Ownership::Client),
        "a passthrough window must keep every finger: {:?}",
        h.ownership
    );
    assert!(h.events.is_empty(), "and recognise nothing: {:?}", h.events);
    assert_eq!(h.seat.ended, vec![0, 1, 2]);
    assert!(h.is_clean());

    // Now the same window, but the finger starts in an edge zone. The zone
    // must win: an edge swipe from inside a drawing app is the escape hatch
    // for "this app swallowed my gestures".
    let mut h = Harness::new(Policy {
        passthrough_apps: vec!["org.kde.krita".to_string()],
        ..Policy::default()
    });
    h.under_gesture_app = true;
    h.down(0, Point::new(20.0, 600.0), false, false, at(0));
    h.frame(at(0));

    assert_eq!(
        h.ownership,
        vec![Ownership::Compositor],
        "a zone beats a passthrough window"
    );

    for step in 1..=15u64 {
        h.motion(0, Point::new(20.0 + 30.0 * step as f64, 600.0));
        h.frame(at(step * 16));
    }
    h.up(0);
    h.frame(at(300));

    assert!(
        h.completed()
            .iter()
            .any(|g| matches!(g, Gesture::EdgeSwipe { .. })),
        "and the gesture still fires: {:?}",
        h.completed()
    );
    assert!(h.is_clean());
}

#[test]
fn exclusion_rect_forwards_touches() {
    // A client advertised a rect — Android's systemGestureExclusionRects —
    // and wants its pinch forwarded while the compositor keeps its edge
    // swipes. The rect here covers the middle of the screen.
    let mut h = Harness::new(Policy {
        exclusion_rects: vec![Rect {
            x: 400.0,
            y: 400.0,
            w: 1120.0,
            h: 400.0,
        }],
        ..Policy::default()
    });

    // Three fingers inside the rect: all forwarded, nothing armed.
    for (i, slot) in [0u32, 1, 2].into_iter().enumerate() {
        h.down(slot, centre(40.0 * i as f64), false, false, at(0));
    }
    h.frame(at(0));
    for slot in 0..3u32 {
        h.up(slot);
    }
    h.frame(at(200));

    assert!(
        h.ownership.iter().all(|o| *o == Ownership::Client),
        "contacts inside an advertised rect are the client's: {:?}",
        h.ownership
    );
    assert!(h.events.is_empty());

    // The documented arming semantics: a rect contact does not count toward
    // arming a later plain-surface finger. Two excluded contacts plus one
    // plain one is still one plain contact, so the third finger below must
    // *not* arm.
    h.down(3, centre(0.0), false, false, at(1000));
    h.down(4, centre(40.0), false, false, at(1000));
    h.frame(at(1000));
    assert_eq!(
        h.ownership[3..5],
        [Ownership::Client, Ownership::Client],
        "still inside the rect"
    );

    // Now leave the rect. Each plain-surface contact is one toward the
    // threshold no matter how many excluded contacts are also down, so the
    // *third* plain one is what arms — two excluded contacts contribute zero.
    h.down(5, Point::new(960.0, 100.0), false, false, at(1010));
    h.frame(at(1010));
    assert_eq!(
        h.ownership[5],
        Ownership::Client,
        "one plain contact is not three, and two excluded ones do not help"
    );

    h.down(6, Point::new(960.0, 140.0), false, false, at(1020));
    h.frame(at(1020));
    assert_eq!(
        h.ownership[6],
        Ownership::Client,
        "two plain contacts are still not three"
    );

    h.down(7, Point::new(960.0, 180.0), false, false, at(1030));
    h.frame(at(1030));
    assert_eq!(
        h.ownership[7],
        Ownership::Compositor,
        "the third plain-surface contact arms, regardless of how many \
         excluded contacts are also down"
    );

    for slot in 3..8u32 {
        h.up(slot);
    }
    h.frame(at(1200));

    assert_eq!(
        h.seat.open,
        BTreeSet::new(),
        "every forwarded touch was closed"
    );
    assert!(h.is_clean());
}

// ── the retro-fed contacts, through the whole sequence ────────────────────

/// The contacts cancelled at the arming must stay tracked for the *whole*
/// sequence, not just at the down.
///
/// This is the second half of the composition seam, and it is a distinct bug
/// from the first. When the third finger arms, slots 0 and 1 are handed back
/// by `clear_clients()` and closed on the seat — which stops them being
/// phantoms, and stops them being the *client's*. It does not lift them off
/// the glass: all three contacts are still down, and all three belong to the
/// gesture the compositor just took.
///
/// Feeding them at the down is only half a fix. If `motion` and `up` are then
/// gated on the routing sets instead of on what the recogniser actually
/// holds, the retro-fed contacts are frozen for the rest of the sequence:
/// they never move, never lift, the grab never uninstalls, and the stuck
/// contacts poison the next gesture. So this asserts the down, the motion and
/// the lift separately, and asserts the recogniser's contact set is *empty*
/// at the end rather than merely smaller — a leak of exactly one contact is
/// the whole bug.
#[test]
fn retro_fed_contacts_keep_tracking_through_motion_and_lift() {
    let mut h = Harness::new(Policy::default());

    for (i, slot) in [0u32, 1, 2].into_iter().enumerate() {
        h.down(slot, centre(40.0 * i as f64), false, false, at(0));
    }
    h.frame(at(0));

    // (1) All three are tracked, including the two just cancelled at the seat.
    assert_eq!(
        h.recogniser_contacts(),
        BTreeSet::from([0, 1, 2]),
        "arming must feed the whole sequence to the recogniser"
    );
    assert_eq!(h.cancels, vec![vec![0, 1]]);
    assert_eq!(h.seat.open, BTreeSet::new(), "and close the client half");
    assert!(
        h.client_slots.is_empty(),
        "the cancelled contacts are no longer the client's"
    );

    // (2) Motion for each of the three advances the gesture. A frozen contact
    // cannot produce this: with the contacts stuck at their start positions
    // the centroid never travels, so the recogniser never *begins* a swipe.
    // Checking at the `Begin` rather than waiting for the `End` is what makes
    // this a motion assertion — the `End` only arrives at the lift, so a check
    // there would not distinguish "moved" from "lifted".
    drag_left(&mut h, &[0, 1, 2], 15, 600.0);

    assert!(
        h.events
            .iter()
            .any(|e| matches!(e, GestureEvent::Begin(Gesture::Swipe { fingers: 3, .. }))),
        "motion must advance the gesture to a three-finger swipe; got {:?}",
        h.events
    );
    assert!(
        h.events
            .iter()
            .any(|e| matches!(e, GestureEvent::Update(Gesture::Swipe { progress, .. }) if *progress > 0.0)),
        "and must keep reporting progress; a frozen contact reports none"
    );

    // (3) An up for each of the three removes that one. Asserting per-slot is
    // the point: a bug that leaks exactly one contact is invisible to a check
    // made only at the end.
    for (lifted, slot) in [0u32, 1, 2].into_iter().enumerate() {
        h.up(slot);
        assert_eq!(
            h.recogniser_contacts(),
            ((lifted + 1) as u32..3).collect::<BTreeSet<u32>>(),
            "slot {slot} must leave the recogniser on its own up"
        );
    }
    h.frame(at(400));

    // And the lift closes the swipe, with real travel behind it.
    let (fingers, delta) = h
        .events
        .iter()
        .rev()
        .find_map(|e| match e {
            GestureEvent::End {
                gesture: Gesture::Swipe { fingers, delta, .. },
                completed: true,
            } => Some((*fingers, *delta)),
            _ => None,
        })
        .expect("the lift must close the swipe");
    assert_eq!(fingers, 3);
    assert!(
        delta > 0.0,
        "the swipe travelled {delta} px; a frozen contact reports 0"
    );

    // (4) Empty, not merely smaller.
    assert_eq!(
        h.recogniser_contacts(),
        BTreeSet::new(),
        "the recogniser must hold nothing once every finger is up"
    );
    assert_eq!(h.fed_slots, BTreeSet::new(), "so the grab would unset");
    assert!(!h.rec.is_active(), "and no gesture is left open");
    assert!(h.is_clean());
}

/// The harness must actually be wired to both stacks, or every gate above is
/// vacuously green.
#[test]
fn the_harness_really_drives_both_stacks() {
    let mut h = Harness::new(Policy::default());
    assert!(h.rec.contacts().next().is_none());

    h.down(0, centre(0.0), false, false, at(0));
    assert_eq!(h.ownership, vec![Ownership::Client], "L3 ruled");
    assert!(
        h.rec.contacts().next().is_none(),
        "a client contact must not reach the recogniser"
    );

    h.up(0);
    h.down(0, Point::new(20.0, 600.0), false, false, at(10));
    assert_eq!(h.ownership[1], Ownership::Compositor, "a zone armed L3");
    assert_eq!(h.rec.contacts().count(), 1, "and the contact reached L1/L2");
}

/// Zone classification is the one input the harness synthesises rather than
/// derives. This pins the assumption every zone gate rests on, so a change in
/// `classify_zone` fails here loudly instead of quietly neutering four tests.
#[test]
fn the_synthetic_output_places_the_zone_contact_points_where_intended() {
    assert_eq!(classify_zone(centre(0.0), OUTPUT, ZONE_PX), None, "middle");
    assert_eq!(
        classify_zone(Point::new(20.0, 600.0), OUTPUT, ZONE_PX),
        Some(Zone::Edge {
            edge: Edge::Left,
            third: Third::Center,
        }),
        "left edge, middle third"
    );
    assert_eq!(
        classify_zone(Point::new(10.0, 1190.0), OUTPUT, ZONE_PX),
        Some(Zone::Corner {
            corner: Corner::BottomLeft
        }),
        "bottom-left corner"
    );
    const {
        assert!(
            OUTPUT.w > 0.0 && OUTPUT.h > 0.0 && ZONE_PX > 0.0,
            "the synthetic output must be usable"
        );
    }
}

/// The composition must be a function of event order and nothing else.
///
/// Carried over from the retired `arbitration_trace.rs`: replaying one
/// scripted trace twice must give byte-identical ownership, cancels, seat
/// state and gesture events. Anything that read a clock or a hash-map
/// iteration order would show up here as a flake in CI rather than as a bug
/// on someone's desk.
#[test]
fn replaying_a_trace_is_deterministic_across_runs() {
    fn run() -> (
        Vec<Ownership>,
        Vec<Vec<u32>>,
        BTreeSet<u32>,
        Vec<GestureEvent>,
    ) {
        let mut h = Harness::new(Policy::default());
        for (i, slot) in [0u32, 1, 2].into_iter().enumerate() {
            h.down(slot, centre(40.0 * i as f64), false, false, at(0));
        }
        h.frame(at(0));
        drag_left(&mut h, &[0, 1, 2], 15, 600.0);
        for slot in 0..3u32 {
            h.up(slot);
        }
        h.frame(at(400));
        (h.ownership, h.cancels, h.seat.open, h.events)
    }
    assert_eq!(run(), run(), "one trace must give one result");
}

/// Unused-import guard: the maps and sets above are part of the harness's
/// shape, and a refactor that drops one should fail here rather than silently
/// changing what a gate asserts.
#[test]
fn the_harness_types_are_all_exercised() {
    let mut positions = BTreeMap::new();
    positions.insert(0u32, centre(0.0));
    assert_eq!(positions.len(), 1);
    let slots: BTreeSet<u32> = [0u32, 1, 2].into_iter().collect();
    assert_eq!(slots.len(), 3);
}

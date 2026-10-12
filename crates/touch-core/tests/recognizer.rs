//! L2 recogniser behaviour.
//!
//! Drives [`Recognizer`] through its public API only, with no compositor and
//! no clock — see `golden_trace.rs` for why that matters.

use std::time::Duration;

use touch_core::{
    Corner, Edge, Gesture, GestureEvent, PinchDirection, Point, Recognizer, RecognizerConfig,
    RotateDirection, Slot, SwipeDirection, Third, Zone,
};

/// Drives a recogniser with a synthetic clock and a tidier event surface.
struct Harness {
    r: Recognizer,
    ms: u64,
    log: Vec<GestureEvent>,
}

impl Harness {
    fn new() -> Self {
        Self::with(RecognizerConfig::default())
    }

    fn with(cfg: RecognizerConfig) -> Self {
        Self {
            r: Recognizer::new(cfg),
            ms: 0,
            log: Vec::new(),
        }
    }

    fn down(&mut self, slot: u32, x: f64, y: f64, zone: Option<Zone>) {
        self.r
            .down(Slot::new(slot), Point::new(x, y), zone, self.t());
    }

    fn move_to(&mut self, slot: u32, x: f64, y: f64) {
        assert!(
            self.r.motion(Slot::new(slot), Point::new(x, y)),
            "slot {slot}"
        );
    }

    fn up(&mut self, slot: u32) {
        self.r.up(Slot::new(slot));
    }

    fn advance(&mut self, ms: u64) {
        self.ms += ms;
    }

    fn t(&self) -> Duration {
        Duration::from_millis(self.ms)
    }

    fn frame(&mut self) -> &[GestureEvent] {
        self.log.clear();
        let evs = self.r.frame(self.t());
        self.log.extend(evs);
        &self.log
    }

    /// Seal a frame, appending to the log instead of replacing it.
    ///
    /// `frame` is per-frame state and clears, which is what most tests want
    /// and what keeps them short. A test that drives a whole gesture and
    /// then asks what it produced needs the opposite: the recogniser emits
    /// events over many frames, and clearing between them loses everything
    /// but the last one.
    fn frame_accum(&mut self) {
        let evs = self.r.frame(self.t());
        self.log.extend(evs);
    }

    /// Every event seen since the harness was built.
    fn all(&self) -> Vec<GestureEvent> {
        self.log.clone()
    }

    fn frame_at(&mut self, ms: u64) -> &[GestureEvent] {
        self.ms = ms;
        self.frame()
    }

    fn reset(&mut self) -> Vec<GestureEvent> {
        self.r.reset()
    }

    /// Translate every live contact by `(dx, dy)`, then seal a frame.
    fn drag_all(&mut self, dx: f64, dy: f64) {
        let live: Vec<(u32, f64, f64)> = self
            .r
            .contacts()
            .map(|(s, c)| (s.id(), c.pos.x, c.pos.y))
            .collect();
        for (slot, x, y) in live {
            self.move_to(slot, x + dx, y + dy);
        }
        self.advance(16);
        self.frame();
    }

    fn three_fingers(&mut self) {
        self.down(0, 500.0, 500.0, None);
        self.down(1, 500.0, 560.0, None);
        self.down(2, 500.0, 620.0, None);
        self.frame();
    }
}

fn begin_of(evs: impl AsRef<[GestureEvent]>) -> Vec<Gesture> {
    evs.as_ref()
        .iter()
        .filter_map(|e| match e {
            GestureEvent::Begin(g) => Some(*g),
            _ => None,
        })
        .collect()
}

/// Every `End` event as `(gesture, completed)`.
///
/// `completed` is the only field L3 acts on — it fires an action on a
/// completed `End` and nothing otherwise — so it is what these tests assert.
fn ends_of(evs: impl AsRef<[GestureEvent]>) -> Vec<(Gesture, bool)> {
    evs.as_ref()
        .iter()
        .filter_map(|e| match e {
            GestureEvent::End { gesture, completed } => Some((*gesture, *completed)),
            _ => None,
        })
        .collect()
}
#[test]
fn clean_three_finger_tap_fires_on_lift() {
    let mut h = Harness::new();
    h.three_fingers();
    assert!(h.frame().is_empty(), "nothing should fire while resting");

    h.up(0);
    h.up(1);
    h.up(2);
    assert_eq!(
        h.frame(),
        [
            GestureEvent::Begin(Gesture::Tap { fingers: 3 }),
            GestureEvent::End {
                gesture: Gesture::Tap { fingers: 3 },
                completed: true
            }
        ]
    );
}

#[test]
fn tap_reports_the_peak_finger_count() {
    // Two land, two more join, all lift: the tap is a four-finger tap.
    let mut h = Harness::new();
    h.down(0, 500.0, 500.0, None);
    h.down(1, 500.0, 560.0, None);
    h.frame();
    h.down(2, 500.0, 620.0, None);
    h.down(3, 500.0, 680.0, None);
    h.frame();
    for s in 0..4 {
        h.up(s);
    }
    assert_eq!(begin_of(h.frame()), [Gesture::Tap { fingers: 4 }]);
}

#[test]
fn drift_past_the_wobble_budget_kills_the_tap() {
    let mut h = Harness::new();
    h.down(0, 500.0, 500.0, None);
    h.down(1, 500.0, 560.0, None);
    h.frame();

    h.move_to(0, 500.0, 540.0);
    h.advance(16);
    let evs = h.frame();

    // Below the swipe threshold, so the only event is the tap candidate dying.
    assert_eq!(
        begin_of(evs),
        [Gesture::TapHold {
            fingers: 2,
            direction: Some(SwipeDirection::Down)
        }]
    );

    // And lifting now produces no tap.
    h.up(0);
    h.up(1);
    assert!(h.frame().is_empty(), "a killed tap must not resurrect");
}

#[test]
fn resting_past_the_duration_budget_kills_the_tap() {
    let mut h = Harness::new();
    h.down(0, 500.0, 500.0, None);
    h.down(1, 500.0, 560.0, None);
    h.frame();

    // Default budget is 500 ms; sit still well past it.
    h.frame_at(900);
    h.up(0);
    h.up(1);
    assert!(h.frame().is_empty(), "a stale tap must not fire");
}

#[test]
fn tap_candidate_dying_precedes_the_follow_on_gesture() {
    let mut h = Harness::new();
    h.three_fingers();

    // Past the wobble budget *and* past the swipe threshold in one step.
    h.drag_all(-80.0, 0.0);

    let begins = begin_of(&h.log);
    assert_eq!(
        begins,
        [
            Gesture::TapHold {
                fingers: 3,
                direction: Some(SwipeDirection::Left)
            },
            Gesture::Swipe {
                fingers: 3,
                direction: SwipeDirection::Left,
                delta: 80.0,
                progress: 80.0 / 300.0
            },
        ],
        "the tap candidate must die before a gesture is recognised"
    );
}

// ── swipe ──────────────────────────────────────────────────────────────────

#[test]
fn swipe_locks_left_and_tracks_progress() {
    let mut h = Harness::new();
    h.three_fingers();

    h.drag_all(-60.0, 0.0);
    // 60 px clears the wobble budget *and* the swipe threshold, so the tap
    // candidate dies and the swipe locks on the same frame — in that order.
    assert_eq!(
        begin_of(&h.log),
        [
            Gesture::TapHold {
                fingers: 3,
                direction: Some(SwipeDirection::Left)
            },
            Gesture::Swipe {
                fingers: 3,
                direction: SwipeDirection::Left,
                delta: 60.0,
                progress: 0.2
            },
        ]
    );
    h.drag_all(-120.0, 0.0);
    let last = h.log.last().copied().expect("update");
    match last {
        GestureEvent::Update(Gesture::Swipe {
            delta, progress, ..
        }) => {
            assert!((delta - 180.0).abs() < 1e-9, "delta {delta}");
            assert!(
                (progress - 180.0 / 300.0).abs() < 1e-9,
                "progress {progress}"
            );
        }
        other => panic!("expected a swipe update, got {other:?}"),
    }
}

#[test]
fn swipe_crossing_the_completion_distance_ends_completed() {
    let mut h = Harness::new();
    h.three_fingers();
    h.drag_all(-320.0, 0.0);
    for s in 0..3 {
        h.up(s);
    }
    let evs = h.frame();
    let last = evs.last().copied().expect("end");
    match last {
        GestureEvent::End { gesture, completed } => {
            assert!(completed, "a 320px swipe should complete");
            assert_eq!(gesture.progress(), Some(1.0));
        }
        other => panic!("expected an End, got {other:?}"),
    }
}

#[test]
fn swipe_below_the_completion_distance_ends_incomplete() {
    let mut h = Harness::new();
    h.three_fingers();
    h.drag_all(-60.0, 0.0);
    for s in 0..3 {
        h.up(s);
    }
    let evs = h.frame();
    let last = evs.last().copied().expect("end");
    match last {
        GestureEvent::End { completed, .. } => assert!(!completed, "60px must not complete"),
        other => panic!("expected an End, got {other:?}"),
    }
}

#[test]
fn axis_locks_so_a_diagonal_drag_keeps_one_direction() {
    let mut h = Harness::new();
    h.three_fingers();

    // Clearly horizontal first.
    h.drag_all(-100.0, 0.0);
    let first = begin_of(&h.log)
        .into_iter()
        .find(|g| matches!(g, Gesture::Swipe { .. }))
        .expect("swipe");

    // Now drag mostly vertically. The direction must stay locked.
    h.drag_all(-1.0, 200.0);
    let updates: Vec<_> = h
        .log
        .iter()
        .filter_map(|e| match e {
            GestureEvent::Update(Gesture::Swipe {
                direction, delta, ..
            }) => Some((*direction, *delta)),
            _ => None,
        })
        .collect();
    assert!(!updates.is_empty(), "expected updates");
    for (dir, _) in &updates {
        assert_eq!(*dir, SwipeDirection::Left, "direction must not drift");
    }
    // Delta stays on the locked horizontal axis.
    let (_, last_delta) = *updates.last().expect("update");
    assert!(
        (last_delta - 101.0).abs() < 1e-9,
        "delta should track the horizontal axis only, got {last_delta}"
    );

    match first {
        Gesture::Swipe { direction, .. } => assert_eq!(direction, SwipeDirection::Left),
        other => panic!("expected a swipe, got {other:?}"),
    }
}

#[test]
fn a_swipe_never_becomes_a_pinch() {
    let mut h = Harness::new();
    h.three_fingers();
    h.drag_all(-100.0, 0.0);
    // Spread the fingers out hard while still moving left.
    h.move_to(0, 500.0, 200.0);
    h.move_to(1, 500.0, 640.0);
    h.move_to(2, 500.0, 920.0);
    h.advance(16);
    h.frame();

    for ev in &h.log {
        if let GestureEvent::Begin(g) | GestureEvent::Update(g) = ev {
            assert!(
                !matches!(g, Gesture::Pinch { .. }),
                "a locked swipe must stay a swipe, got {g:?}"
            );
        }
    }
}

#[test]
fn single_finger_mid_screen_is_not_a_swipe() {
    let mut h = Harness::new();
    h.down(0, 500.0, 500.0, None);
    h.frame();
    h.drag_all(-300.0, 0.0);

    for ev in &h.log {
        if let GestureEvent::Begin(g) | GestureEvent::Update(g) = ev {
            assert!(
                !matches!(g, Gesture::Swipe { .. }),
                "one finger must not drive a multi-finger swipe, got {g:?}"
            );
        }
    }
}

// ── pinch ──────────────────────────────────────────────────────────────────

#[test]
fn pinch_out_is_recognised_and_tracks_spread() {
    let mut h = Harness::new();
    h.down(0, 400.0, 500.0, None);
    h.down(1, 600.0, 500.0, None);
    h.frame();

    // Spread apart about a fixed midpoint: centroid still, spread grows.
    h.move_to(0, 350.0, 500.0);
    h.move_to(1, 650.0, 500.0);
    h.advance(16);
    h.frame();

    let pinch = begin_of(&h.log)
        .into_iter()
        .find(|g| matches!(g, Gesture::Pinch { .. }))
        .expect("pinch out should be recognised");
    match pinch {
        Gesture::Pinch {
            direction,
            delta,
            fingers,
            ..
        } => {
            assert_eq!(direction, PinchDirection::Out);
            assert_eq!(fingers, 2);
            assert!(delta > 0.0, "diverging means positive spread delta");
        }
        other => panic!("expected a pinch, got {other:?}"),
    }
}

#[test]
fn pinch_in_is_the_mirror_image() {
    let mut h = Harness::new();
    h.down(0, 300.0, 500.0, None);
    h.down(1, 700.0, 500.0, None);
    h.frame();

    h.move_to(0, 450.0, 500.0);
    h.move_to(1, 550.0, 500.0);
    h.advance(16);
    h.frame();

    let pinch = begin_of(&h.log)
        .into_iter()
        .find(|g| matches!(g, Gesture::Pinch { .. }))
        .expect("pinch in should be recognised");
    match pinch {
        Gesture::Pinch {
            direction, delta, ..
        } => {
            assert_eq!(direction, PinchDirection::In);
            assert!(delta < 0.0);
        }
        other => panic!("expected a pinch, got {other:?}"),
    }
}

#[test]
fn translation_beats_an_incidental_spread_change() {
    // Fingers splay slightly while the whole cluster moves: the swipe is
    // the intended gesture, and it is also further past its threshold.
    let mut h = Harness::new();
    h.three_fingers();
    h.move_to(0, 500.0, 560.0);
    h.move_to(1, 500.0, 600.0);
    h.move_to(2, 500.0, 660.0);
    h.advance(16);
    // This frame clears the wobble budget and the swipe threshold at once.
    // Spread moves by under 5 px, so only travel could have crossed.
    let begins = begin_of(h.frame());

    let swipe = begins
        .iter()
        .find(|g| matches!(g, Gesture::Swipe { .. }))
        .copied()
        .unwrap_or_else(|| panic!("translation should win; began with {begins:?}"));
    assert!(
        matches!(
            swipe,
            Gesture::Swipe {
                direction: SwipeDirection::Down,
                ..
            }
        ),
        "expected a downward swipe, got {swipe:?}"
    );

    h.move_to(0, 380.0, 560.0);
    h.move_to(1, 380.0, 600.0);
    h.move_to(2, 380.0, 660.0);
    h.advance(16);
    let evs = h.frame().to_vec();
    assert!(
        !begin_of(&evs)
            .iter()
            .any(|g| matches!(g, Gesture::Pinch { .. })),
        "a locked swipe must not turn into a pinch"
    );
}

// ── rotation ───────────────────────────────────────────────────────────────

#[test]
fn counter_clockwise_rotation_is_recognised() {
    let mut h = Harness::new();
    ring(&mut h);
    h.frame();

    rotate_ring(&mut h, -0.6);
    h.frame();

    let rot = begin_of(&h.log)
        .into_iter()
        .find(|g| matches!(g, Gesture::Rotate { .. }))
        .expect("rotation should be recognised");
    match rot {
        Gesture::Rotate {
            direction,
            delta,
            fingers,
            ..
        } => {
            assert_eq!(direction, RotateDirection::CounterClockwise);
            assert_eq!(fingers, 4);
            assert!(delta > 0.0, "rotation magnitude is non-negative");
        }
        other => panic!("expected a rotation, got {other:?}"),
    }
}

#[test]
fn clockwise_rotation_is_recognised() {
    let mut h = Harness::new();
    ring(&mut h);
    h.frame();

    rotate_ring(&mut h, 0.6);
    h.frame();

    let rot = begin_of(&h.log)
        .into_iter()
        .find(|g| matches!(g, Gesture::Rotate { .. }))
        .expect("rotation should be recognised");
    match rot {
        Gesture::Rotate { direction, .. } => assert_eq!(direction, RotateDirection::Clockwise),
        other => panic!("expected a rotation, got {other:?}"),
    }
}

#[test]
fn collinear_fingers_drifting_down_swipe_rather_than_rotate() {
    // Regression: three contacts stacked in a vertical line are the ordinary
    // shape of a 3-finger vertical swipe. Their per-slot angles are
    // ill-conditioned — the centre contact sits exactly on the centroid and
    // its angle flips between +π/2 and -π/2 as the centroid moves — which
    // yielded a phantom rotation of ~π/6, over 3× the rotate threshold and
    // enough to beat the real swipe on relative overshoot.
    let mut h = Harness::new();
    h.down(0, 500.0, 500.0, None);
    h.down(1, 500.0, 560.0, None);
    h.down(2, 500.0, 620.0, None);
    h.frame();

    h.drag_all(0.0, 80.0);
    let begins = begin_of(&h.log);
    let swipe = begins
        .iter()
        .find(|g| matches!(g, Gesture::Swipe { .. }))
        .copied()
        .unwrap_or_else(|| panic!("expected a swipe, began with {begins:?}"));
    assert!(
        matches!(
            swipe,
            Gesture::Swipe {
                direction: SwipeDirection::Down,
                fingers: 3,
                ..
            }
        ),
        "expected a 3-finger downward swipe, got {swipe:?}"
    );
}

#[test]
fn collinear_fingers_drifting_left_also_swipe() {
    let mut h = Harness::new();
    h.down(0, 500.0, 500.0, None);
    h.down(1, 600.0, 500.0, None);
    h.down(2, 700.0, 500.0, None);
    h.frame();

    h.drag_all(-80.0, 0.0);
    let begins = begin_of(&h.log);
    assert!(
        begins.iter().any(|g| matches!(
            g,
            Gesture::Swipe {
                direction: SwipeDirection::Left,
                ..
            }
        )),
        "expected a leftward swipe, began with {begins:?}"
    );
}

#[test]
fn rigid_rotation_leaves_the_centroid_alone() {
    let mut h = Harness::new();
    ring(&mut h);
    h.frame();
    rotate_ring(&mut h, -0.6);
    h.frame();

    for ev in &h.log {
        if let GestureEvent::Begin(g) | GestureEvent::Update(g) = ev {
            assert!(
                !matches!(g, Gesture::Swipe { .. } | Gesture::Pinch { .. }),
                "rotation must not be mistaken for translation: {g:?}"
            );
        }
    }
}

// ── edge and corner ────────────────────────────────────────────────────────

#[test]
fn single_finger_from_the_bottom_edge_locks_an_edge_swipe() {
    let mut h = Harness::new();
    let zone = Zone::Edge {
        edge: Edge::Bottom,
        third: Third::Center,
    };
    h.down(0, 500.0, 1275.0, Some(zone));
    h.frame();

    h.drag_all(0.0, -60.0);
    let edge = begin_of(&h.log)
        .into_iter()
        .find(|g| matches!(g, Gesture::EdgeSwipe { .. }))
        .expect("edge swipe should be recognised");
    match edge {
        Gesture::EdgeSwipe { edge, third, .. } => {
            assert_eq!(edge, Edge::Bottom);
            assert_eq!(third, Third::Center);
        }
        other => panic!("expected an edge swipe, got {other:?}"),
    }
}

#[test]
fn corner_zone_locks_a_corner_swipe() {
    let mut h = Harness::new();
    let zone = Zone::Corner {
        corner: Corner::BottomLeft,
    };
    h.down(0, 5.0, 1275.0, Some(zone));
    h.frame();

    h.drag_all(60.0, -60.0);
    let c = begin_of(&h.log)
        .into_iter()
        .find(|g| matches!(g, Gesture::CornerSwipe { .. }))
        .expect("corner swipe should be recognised");
    match c {
        Gesture::CornerSwipe { corner, .. } => assert_eq!(corner, Corner::BottomLeft),
        other => panic!("expected a corner swipe, got {other:?}"),
    }
}

#[test]
fn edge_zones_are_ignored_once_a_second_finger_lands() {
    let mut h = Harness::new();
    let zone = Zone::Edge {
        edge: Edge::Bottom,
        third: Third::Center,
    };
    h.down(0, 500.0, 1275.0, Some(zone));
    h.frame();
    h.down(1, 600.0, 1200.0, None);
    h.frame();

    h.drag_all(0.0, -80.0);
    for ev in &h.log {
        if let GestureEvent::Begin(g) | GestureEvent::Update(g) = ev {
            assert!(
                !matches!(g, Gesture::EdgeSwipe { .. } | Gesture::CornerSwipe { .. }),
                "edge gestures are single-finger: {g:?}"
            );
        }
    }
}

/// An edge swipe must reach `progress: 1.0` (`completed: true`) over a
/// realistic inward drag, because L3 fires an action only on a *completed*
/// `End`. If it cannot, the bind silently never fires and edge swipes look
/// dead on hardware even though the zone was classified and the gesture was
/// recognised — which is exactly the failure this pins.
///
/// The geometry here is the real one from the bug report: a 1066x711 logical
/// panel, a finger landing 21 logical px from the left edge, dragged inward
/// the ~215 px a deliberate swipe covers.
#[test]
fn a_realistic_edge_swipe_reaches_the_completion_threshold() {
    /// The real geometry from the bug report: a 1066x711 logical panel, a
    /// finger landing on the bottom edge, dragged inward.
    fn drag(inward_px: f64) -> Vec<GestureEvent> {
        let mut h = Harness::new();
        h.down(
            0,
            533.0,
            711.0,
            Some(Zone::Edge {
                edge: Edge::Bottom,
                third: Third::Center,
            }),
        );
        h.frame_accum();
        // In small steps, the way a real finger arrives: a single large
        // jump is not what the digitiser sends.
        let steps = 40;
        for _ in 0..steps {
            h.drag_all(0.0, -inward_px / steps as f64);
            h.frame_accum();
        }
        h.up(0);
        h.frame_accum();
        h.all()
    }

    let completion_px = RecognizerConfig::default().progress_distance;

    // Short of the completion distance: recognised, but not completed.
    // Scoped to the edge swipe — the same motion also emits a completed
    // `TapHold`, which is a different gesture on a different bind.
    let short: Vec<_> = ends_of(drag(completion_px * 0.5))
        .into_iter()
        .filter(|(g, _)| matches!(g, Gesture::EdgeSwipe { .. }))
        .collect();
    assert!(!short.is_empty(), "the edge swipe must still be recognised");
    assert!(
        short.iter().all(|(_, completed)| !*completed),
        "a swipe of half the completion distance must not report completion, got {short:?}"
    );

    // The same swipe, carried to the full completion distance.
    let full: Vec<_> = ends_of(drag(completion_px))
        .into_iter()
        .filter(|(g, _)| matches!(g, Gesture::EdgeSwipe { .. }))
        .collect();
    assert!(!full.is_empty(), "the edge swipe must still be recognised");
    assert!(
        full.iter().any(|(_, completed)| *completed),
        "an edge swipe of the full completion distance must report completion, got {full:?}"
    );
}

// ── finger-count bounds ────────────────────────────────────────────────────

// At the default `min_fingers: 2` the single-finger early return already
// excludes everything the bound would, so these two tests raise or lower it
// to values where the bound is the *only* thing doing the work.

#[test]
fn min_fingers_above_two_is_enforced() {
    let cfg = RecognizerConfig {
        min_fingers: 3,
        ..RecognizerConfig::default()
    };

    // Two fingers cannot reach a three-finger minimum.
    let mut h = Harness::with(cfg);
    h.down(0, 500.0, 500.0, None);
    h.down(1, 500.0, 560.0, None);
    h.frame();
    h.drag_all(0.0, -100.0);
    let begins = begin_of(&h.log);
    assert!(
        !begins.iter().any(|g| matches!(g, Gesture::Swipe { .. })),
        "two fingers must not satisfy min_fingers=3, began with {begins:?}"
    );

    // Three fingers can.
    let mut h = Harness::with(cfg);
    h.three_fingers();
    h.drag_all(0.0, -100.0);
    let begins = begin_of(&h.log);
    assert!(
        begins.iter().any(|g| matches!(g, Gesture::Swipe { .. })),
        "three fingers should satisfy min_fingers=3, began with {begins:?}"
    );
}

#[test]
fn max_fingers_is_enforced() {
    let cfg = RecognizerConfig {
        max_fingers: 2,
        ..RecognizerConfig::default()
    };
    let mut h = Harness::with(cfg);
    h.three_fingers();
    h.drag_all(-100.0, 0.0);
    let begins = begin_of(&h.log);
    assert!(
        !begins.iter().any(|g| matches!(g, Gesture::Swipe { .. })),
        "three fingers must not satisfy max_fingers=2, began with {begins:?}"
    );
}

// ── lifecycle invariant ────────────────────────────────────────────────────

/// Assert that every `Begin` is matched by exactly one `End`, and that no
/// `Update` escapes an active gesture.
fn assert_well_formed(evs: &[GestureEvent]) {
    let mut open: Option<Gesture> = None;
    for ev in evs {
        match ev {
            GestureEvent::Begin(g) => {
                assert!(open.is_none(), "Begin while {:?} still open", open);
                open = Some(*g);
            }
            GestureEvent::Update(g) => {
                assert!(open.is_some(), "Update outside any Begin: {g:?}");
                let cur = open.expect("open");
                assert!(
                    discriminant(&cur) == discriminant(g),
                    "Update changed gesture kind: {cur:?} → {g:?}"
                );
            }
            GestureEvent::End { gesture, .. } => {
                assert!(open.is_some(), "End without a matching Begin: {gesture:?}");
                let cur = open.take().expect("open");
                assert_eq!(
                    discriminant(&cur),
                    discriminant(gesture),
                    "End names a different gesture: {cur:?} → {gesture:?}"
                );
            }
        }
    }
    assert!(open.is_none(), "gesture left dangling: {open:?}");
}

fn discriminant(g: &Gesture) -> u8 {
    match g {
        Gesture::Swipe { .. } => 0,
        Gesture::Pinch { .. } => 1,
        Gesture::Rotate { .. } => 2,
        Gesture::Tap { .. } => 3,
        Gesture::TapHold { .. } => 4,
        Gesture::EdgeSwipe { .. } => 5,
        Gesture::CornerSwipe { .. } => 6,
    }
}

#[test]
fn a_three_finger_swipe_is_well_formed() {
    let mut h = Harness::new();
    h.three_fingers();
    let mut all = Vec::new();
    all.extend_from_slice(h.frame());
    for _ in 0..4 {
        h.drag_all(-50.0, 0.0);
        all.extend_from_slice(&h.log);
    }
    for s in 0..3 {
        h.up(s);
    }
    all.extend_from_slice(h.frame());
    assert_well_formed(&all);
}

#[test]
fn reset_during_an_active_swipe_closes_it_without_completing() {
    let mut h = Harness::new();
    h.three_fingers();
    h.drag_all(-100.0, 0.0);
    assert!(h.r.is_active(), "a swipe should be running");

    let out = h.reset();
    match out.as_slice() {
        [GestureEvent::End { gesture, completed }] => {
            assert!(!completed, "a reset gesture must not report completion");
            assert!(matches!(gesture, Gesture::Swipe { .. }));
        }
        other => panic!("expected exactly one End, got {other:?}"),
    }
    assert!(!h.r.is_active());
}

#[test]
fn reset_during_a_tap_candidate_emits_nothing() {
    let mut h = Harness::new();
    h.three_fingers();
    assert!(!h.r.is_active());
    assert!(h.reset().is_empty(), "no gesture to close");
}

#[test]
fn reset_then_a_fresh_sequence_works() {
    let mut h = Harness::new();
    h.three_fingers();
    h.drag_all(-100.0, 0.0);
    h.reset();

    h.down(9, 200.0, 200.0, None);
    h.down(10, 200.0, 260.0, None);
    h.down(11, 200.0, 320.0, None);
    // Accumulate the whole second sequence: `Begin` and its `End` land in
    // different frames, so a single frame can never be well-formed alone.
    let mut all = h.frame().to_vec();
    h.drag_all(0.0, -100.0);
    all.extend_from_slice(&h.log);
    for s in 9..12 {
        h.up(s);
    }
    all.extend_from_slice(h.frame());
    assert_well_formed(&all);
}

#[test]
fn a_late_joining_finger_rebaselines_rather_than_inheriting_travel() {
    let mut h = Harness::new();
    // Two fingers, well off centre, with a little travel.
    h.down(0, 800.0, 500.0, None);
    h.down(1, 800.0, 560.0, None);
    h.frame();
    h.drag_all(-25.0, 0.0);

    // A third lands far away. If the baseline did not move, that alone would
    // blow past the swipe threshold.
    h.down(2, 100.0, 700.0, None);
    h.advance(16);
    let evs = h.frame().to_vec();

    let begins = begin_of(&evs);
    assert!(
        !begins.iter().any(|g| matches!(g, Gesture::Swipe { .. })),
        "re-baselining on the third finger must suppress a phantom swipe, got {begins:?}"
    );
}

// ── helpers ────────────────────────────────────────────────────────────────

fn ring(h: &mut Harness) {
    let r = 100.0_f64;
    for (i, slot) in [0u32, 1, 2, 3].into_iter().enumerate() {
        let a = std::f64::consts::FRAC_PI_2 * i as f64;
        h.down(slot, 500.0 + r * a.cos(), 500.0 + r * a.sin(), None);
    }
}

fn rotate_ring(h: &mut Harness, angle: f64) {
    let (sin, cos) = angle.sin_cos();
    let live: Vec<(u32, f64, f64)> =
        h.r.contacts()
            .map(|(s, c)| (s.id(), c.pos.x, c.pos.y))
            .collect();
    for (slot, x, y) in live {
        h.move_to(
            slot,
            500.0 + (x - 500.0) * cos - (y - 500.0) * sin,
            500.0 + (x - 500.0) * sin + (y - 500.0) * cos,
        );
    }
    h.advance(16);
}

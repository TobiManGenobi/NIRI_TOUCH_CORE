//! Golden-trace harness.
//!
//! This test exists to prove the design thesis of `DESIGN.md`: the touch
//! stack's lower layers are drivable from outside the crate with no
//! compositor, no Wayland connection, no clock and no config. If a future
//! change ever reaches for one of those, this file stops compiling — which
//! is the enforcement, not a comment.
//!
//! A trace is a flat, serialisable list of events with explicit timestamps.
//! Replaying one must produce a deterministic frame sequence, which is what
//! makes golden comparison possible when L2 (the recogniser) lands.

use std::time::Duration;

use touch_core::{classify_zone, ContactSet, Frame, OutputSize, Point, Slot, Zone};

/// One recorded event in a contact trace.
#[derive(Debug, Clone, Copy)]
enum Event {
    Down { slot: u32, x: f64, y: f64 },
    Move { slot: u32, x: f64, y: f64 },
    Up { slot: u32 },
}

/// The timebase used by every trace in this file.
const T0: Duration = Duration::from_millis(0);

fn at(ms: u64) -> Duration {
    Duration::from_millis(ms)
}

/// Replay a trace and collect one frame per `Seal` marker.
///
/// A trace is `(time, Option<Event>)`; `None` seals a frame at that time,
/// mirroring `wl_touch.frame` as the compositor's atomic batch boundary.
fn replay(events: &[(Duration, Option<Event>)]) -> (ContactSet, Vec<Frame>) {
    let mut cs = ContactSet::new();
    let mut frames = Vec::new();

    for (time, event) in events {
        match event {
            Some(Event::Down { slot, x, y }) => {
                cs.down(Slot::new(*slot), Point::new(*x, *y), *time)
            }
            Some(Event::Move { slot, x, y }) => {
                let ok = cs.motion(Slot::new(*slot), Point::new(*x, *y));
                assert!(ok, "motion for untracked slot {slot} — tracker desync");
            }
            Some(Event::Up { slot }) => {
                cs.up(Slot::new(*slot));
            }
            None => frames.push(cs.frame(*time)),
        }
    }

    (cs, frames)
}

/// A three-finger drag sweeping left, sampled every 16 ms.
fn three_finger_drag() -> Vec<(Duration, Option<Event>)> {
    let mut ev = Vec::new();
    let t0 = 0u64;

    // Three fingers land together, 40 px apart vertically.
    for (i, slot) in [0u32, 1, 2].into_iter().enumerate() {
        ev.push((
            at(t0),
            Some(Event::Down {
                slot,
                x: 600.0,
                y: 500.0 + 40.0 * i as f64,
            }),
        ));
    }
    ev.push((at(t0), None));

    // Translate left by 10 px per frame, five times.
    for step in 1..=5u64 {
        for (i, slot) in [0u32, 1, 2].into_iter().enumerate() {
            ev.push((
                at(step * 16),
                Some(Event::Move {
                    slot,
                    x: 600.0 - 10.0 * step as f64,
                    y: 500.0 + 40.0 * i as f64,
                }),
            ));
        }
        ev.push((at(step * 16), None));
    }

    // All three lift together.
    for slot in 0u32..3 {
        ev.push((at(96), Some(Event::Up { slot })));
    }
    ev.push((at(96), None));

    ev
}

#[test]
fn drag_trace_is_deterministic_across_runs() {
    let trace = three_finger_drag();
    let (_, a) = replay(&trace);
    let (_, b) = replay(&trace);
    assert_eq!(a, b, "replaying one trace must give identical frames");
}

#[test]
fn drag_trace_shape() {
    let (_, frames) = replay(&three_finger_drag());

    // 1 landing frame + 5 drag frames + 1 empty frame.
    assert_eq!(frames.len(), 7);
    assert_eq!(frames[0].count, 3);
    for f in &frames[1..6] {
        assert_eq!(f.count, 3);
    }
    assert_eq!(frames[6].count, 0);

    // Rigid translation: spread is invariant, centroid moves left by 10/frame.
    for f in &frames[1..6] {
        assert!(
            (f.spread - 26.666_666_666_666_668).abs() < 1e-9,
            "spread drifted to {}",
            f.spread
        );
    }
    assert!((frames[1].centroid_delta.x + 10.0).abs() < 1e-9);
    assert!((frames[1].centroid_delta.y).abs() < 1e-9);

    // Pure translation is not a rotation.
    for f in &frames {
        assert_eq!(f.rotation_delta, 0.0);
    }
}

#[test]
fn pinch_trace_separates_translation_from_radial_motion() {
    // Two fingers converge about a fixed midpoint. The centroid stays put
    // while the spread shrinks — the signature L2 will key pinch on.
    let mut ev = vec![
        (
            T0,
            Some(Event::Down {
                slot: 0,
                x: 400.0,
                y: 500.0,
            }),
        ),
        (
            T0,
            Some(Event::Down {
                slot: 1,
                x: 600.0,
                y: 500.0,
            }),
        ),
        (T0, None),
    ];
    for step in 1..=3u64 {
        let d = 100.0 - 10.0 * step as f64;
        ev.push((
            at(step * 16),
            Some(Event::Move {
                slot: 0,
                x: 500.0 - d,
                y: 500.0,
            }),
        ));
        ev.push((
            at(step * 16),
            Some(Event::Move {
                slot: 1,
                x: 500.0 + d,
                y: 500.0,
            }),
        ));
        ev.push((at(step * 16), None));
    }

    let (_, frames) = replay(&ev);
    for f in &frames[1..] {
        let c = f.centroid.unwrap();
        assert!((c.x - 500.0).abs() < 1e-9, "centroid drifted to {c:?}");
        assert!((f.centroid_delta.x).abs() < 1e-9, "centroid moved");
    }
    // The fingers converge 10 px per frame, so spread shrinks
    // 100 → 90 → 80 → 70 while the midpoint holds at 500.
    assert!((frames[0].spread - 100.0).abs() < 1e-9);
    assert!((frames[1].spread - 90.0).abs() < 1e-9);
    assert!((frames[2].spread - 80.0).abs() < 1e-9);
    assert!((frames[3].spread - 70.0).abs() < 1e-9);
    // Radial motion is not rotation.
    for f in &frames {
        assert_eq!(f.rotation_delta, 0.0);
    }
}

#[test]
fn rotation_trace_leaves_centroid_and_spread_alone() {
    // Four fingers on a radius-100 circle, turning 90° CCW on screen.
    let radius = 100.0_f64;
    let mut ev = Vec::new();
    for (i, slot) in [0u32, 1, 2, 3].into_iter().enumerate() {
        let a = std::f64::consts::FRAC_PI_2 * i as f64;
        ev.push((
            T0,
            Some(Event::Down {
                slot,
                x: 500.0 + radius * a.cos(),
                y: 500.0 + radius * a.sin(),
            }),
        ));
    }
    ev.push((T0, None));

    let quarter = std::f64::consts::FRAC_PI_2;
    for (i, slot) in [0u32, 1, 2, 3].into_iter().enumerate() {
        let a = std::f64::consts::FRAC_PI_2 * i as f64;
        // Screen y grows downward, so CCW as seen is this rotation.
        let (px, py) = (radius * a.cos(), radius * a.sin());
        let rx = px * quarter.cos() + py * quarter.sin();
        let ry = -px * quarter.sin() + py * quarter.cos();
        ev.push((
            at(16),
            Some(Event::Move {
                slot,
                x: 500.0 + rx,
                y: 500.0 + ry,
            }),
        ));
    }
    ev.push((at(16), None));

    let (_, frames) = replay(&ev);
    assert_eq!(frames.len(), 2);
    assert!(
        (frames[1].rotation_delta - quarter).abs() < 1e-6,
        "got {}",
        frames[1].rotation_delta
    );
    // Rigid rotation: centroid fixed, spread invariant.
    let c = frames[1].centroid.unwrap();
    assert!((c.x - 500.0).abs() < 1e-6 && (c.y - 500.0).abs() < 1e-6);
    assert!((frames[1].spread - frames[0].spread).abs() < 1e-6);
}

#[test]
fn hotplug_mid_gesture_leaves_a_clean_empty_set() {
    // Three fingers down, then the device vanishes. `clear` must report the
    // number of contacts it dropped so L3 can close every client-visible
    // touch it still had open — the invariant patch 0011 of niri-tablet
    // exists because this was once silently mishandled.
    let mut cs = ContactSet::new();
    for (i, slot) in [0u32, 1, 2].into_iter().enumerate() {
        cs.down(
            Slot::new(slot),
            Point::new(600.0, 500.0 + 40.0 * i as f64),
            T0,
        );
    }
    cs.frame(T0);
    assert_eq!(cs.len(), 3);

    assert_eq!(cs.clear(), 3);
    assert!(cs.is_empty());

    let f = cs.frame(at(16));
    assert_eq!(f.count, 0);
    assert_eq!(f.centroid, None);
    assert_eq!(f.spread, 0.0);
    assert_eq!(f.rotation_delta, 0.0);

    // And a fresh sequence afterwards starts from nothing.
    cs.down(Slot::new(0), Point::new(100.0, 100.0), at(32));
    assert_eq!(cs.frame(at(32)).count, 1);
}

#[test]
fn zone_classification_survives_output_rotation() {
    // Same physical gesture, two output shapes. Classification is in output
    // logical coordinates, so the caller must hand us already-transformed
    // points; this asserts the classifier behaves at both aspect ratios.
    let portrait = OutputSize::new(1000.0, 1280.0);
    let landscape = OutputSize::new(1280.0, 1000.0);
    let zone_px = 48.0;

    // Bottom edge, centre third, on both.
    assert_eq!(
        classify_zone(Point::new(500.0, 1279.0), portrait, zone_px),
        Some(Zone::Edge {
            edge: touch_core::Edge::Bottom,
            third: touch_core::Third::Center
        })
    );
    assert_eq!(
        classify_zone(Point::new(640.0, 999.0), landscape, zone_px),
        Some(Zone::Edge {
            edge: touch_core::Edge::Bottom,
            third: touch_core::Third::Center
        })
    );
}

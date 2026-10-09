//! Golden-style traces for the arbitration ledger.
//!
//! Same thesis as `touch-core`'s golden-trace harness: the ownership model
//! must be drivable from outside the crate with no compositor, no seat, no
//! config file and no clock. If a future change reaches for one of those,
//! this file stops compiling — which is the enforcement.
//!
//! Each trace is a `(Duration, Option<Event>)` list. Time is carried
//! explicitly and never read; the arbitrator does not care about it at all,
//! which is precisely the point: ownership must be a function of event
//! *order*, not of timing.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use touch_arbitrate::{
    Arbitrator, DownContext, Ownership, Point, Policy, Rect, DEFAULT_MIN_GESTURE_FINGERS,
};
use touch_core::Zone;

/// One recorded input event.
#[derive(Debug, Clone, Copy)]
enum Event {
    /// A finger contact landed.
    Down { slot: u32, x: f64, y: f64 },
    /// A finger moved. Ownership cannot change mid-contact; carried for
    /// realism and asserted on.
    Move { slot: u32, x: f64, y: f64 },
    /// A finger lifted.
    Up { slot: u32 },
    /// The device went away: every slot is torn down at once.
    DeviceGone,
    /// A Mod binding was pressed or released.
    Mod { down: bool },
    /// The surface under the finger matches (or stops matching) a
    /// `touchscreen-gesture-passthrough` window rule.
    UnderApp { passthrough: bool },
}

/// The timebase used by every trace in this file.
const T0: Duration = Duration::from_millis(0);

fn at(ms: u64) -> Duration {
    Duration::from_millis(ms)
}

/// What a replay produced.
#[derive(Debug)]
struct Replay {
    /// The ownership recorded for each down, in order.
    ownership: Vec<Ownership>,
    /// The ledger left behind.
    arb: Arbitrator,
    /// The count each `DeviceGone` teardown reported.
    torn_down: Vec<usize>,
    /// The slots `clear_clients` cancelled at each arming, in arming order.
    ///
    /// A well-behaved caller cancels the client touches the moment the
    /// compositor takes the sequence over. The harness does it for every
    /// [`Ownership::Compositor`] down so that the traces exercise the real
    /// sequence rather than a ledger that quietly accumulates phantoms.
    cancelled: Vec<Vec<u32>>,
}

/// Replay a trace through an [`Arbitrator`], recording every decision.
fn replay(events: &[(Duration, Option<Event>)], policy: Policy) -> Replay {
    let mut arb = Arbitrator::new(policy);
    let mut ownership = Vec::new();
    let mut torn_down = Vec::new();
    let mut cancelled: Vec<Vec<u32>> = Vec::new();
    let mut cancelled_slots: BTreeSet<u32> = BTreeSet::new();
    let mut mod_down = false;
    let mut under_gesture_app = false;
    let mut zones_at_down: BTreeMap<u32, Option<Zone>> = BTreeMap::new();

    for (_, event) in events {
        match event {
            Some(Event::Down { slot, x, y }) => {
                let ctx = DownContext {
                    // The adapter resolves the rect question against the
                    // policy; here the harness does it so exclusion rects are
                    // drivable from a trace.
                    in_exclusion_rect: arb.policy().excludes(Point::new(*x, *y)),
                    // Zone classification is L1's job; here we take the
                    // adapter's word for it rather than re-deriving.
                    zone: classify(*x, *y),
                    pos: Point::new(*x, *y),
                    under_gesture_app,
                    mod_down,
                    ..DownContext::default()
                };
                let owner = arb.down(*slot, &ctx);
                ownership.push(owner);
                if owner == Ownership::Compositor {
                    // An empty cancel is not a cancel: only record the arms
                    // that actually took something back from the client.
                    let taken = arb.clear_clients();
                    if !taken.is_empty() {
                        cancelled_slots.extend(&taken);
                        cancelled.push(taken);
                    }
                }
                zones_at_down.insert(*slot, ctx.zone);
            }
            Some(Event::Move { slot, x, y }) => {
                // Ownership is decided once, at the down, and never
                // revisited. Assert that across a real zone change: a
                // contact that drifts out of the edge strip it landed in
                // must keep the owner it was given, or the client and the
                // compositor disagree about who has this finger.
                if cancelled_slots.contains(slot) {
                    // The compositor took this contact back at the arming.
                    // The client no longer has it, so the ledger correctly
                    // has no record of it, and the recogniser above is what
                    // follows it from here.
                    zones_at_down.remove(slot);
                    continue;
                }
                let before = arb.ownership(*slot);
                let zone_at_down = zones_at_down.remove(slot);
                assert!(before.is_some(), "motion for untracked slot {slot}");
                if let Some(z0) = zone_at_down {
                    let z1 = classify(*x, *y);
                    if z0 != z1 {
                        assert_eq!(
                            arb.ownership(*slot),
                            before,
                            "slot {slot} drifted from {z0:?} to {z1:?} but \
                             changed owner mid-contact"
                        );
                    }
                }
            }
            Some(Event::Up { slot }) => {
                arb.up(*slot);
            }
            Some(Event::DeviceGone) => {
                torn_down.push(arb.clear());
            }
            Some(Event::Mod { down }) => {
                mod_down = *down;
            }
            Some(Event::UnderApp { passthrough }) => {
                under_gesture_app = *passthrough;
            }
            None => {}
        }
    }

    Replay {
        ownership,
        arb,
        torn_down,
        cancelled,
    }
}

/// Stand-in for L1's `classify_zone`, at niri-tablet's 48 px edge strip.
fn classify(x: f64, y: f64) -> Option<Zone> {
    const W: f64 = 1920.0;
    const H: f64 = 1200.0;
    const EDGE: f64 = 48.0;

    if x < EDGE {
        Some(Zone::Edge {
            edge: touch_core::Edge::Left,
            third: if y < H / 3.0 {
                touch_core::Third::Start
            } else if y < 2.0 * H / 3.0 {
                touch_core::Third::Center
            } else {
                touch_core::Third::End
            },
        })
    } else if x > W - EDGE {
        Some(Zone::Edge {
            edge: touch_core::Edge::Right,
            third: if y < H / 3.0 {
                touch_core::Third::Start
            } else if y < 2.0 * H / 3.0 {
                touch_core::Third::Center
            } else {
                touch_core::Third::End
            },
        })
    } else if y < EDGE {
        Some(Zone::Edge {
            edge: touch_core::Edge::Top,
            third: if x < W / 3.0 {
                touch_core::Third::Start
            } else if x < 2.0 * W / 3.0 {
                touch_core::Third::Center
            } else {
                touch_core::Third::End
            },
        })
    } else if y > H - EDGE {
        Some(Zone::Edge {
            edge: touch_core::Edge::Bottom,
            third: if x < W / 3.0 {
                touch_core::Third::Start
            } else if x < 2.0 * W / 3.0 {
                touch_core::Third::Center
            } else {
                touch_core::Third::End
            },
        })
    } else {
        None
    }
}

fn three_finger_swipe() -> Vec<(Duration, Option<Event>)> {
    let mut ev = Vec::new();
    // Three fingers land together, mid-screen, nowhere near an edge: the
    // naive reading of this gesture is "three client taps".
    for i in 0u32..3 {
        ev.push((
            T0,
            Some(Event::Down {
                slot: i,
                x: 600.0,
                y: 500.0 + 40.0 * f64::from(i),
            }),
        ));
    }
    ev.push((T0, None));
    // Drag left across the screen over 10 frames.
    for step in 1..=10u64 {
        for i in 0u32..3 {
            ev.push((
                at(step * 16),
                Some(Event::Move {
                    slot: i,
                    x: 600.0 - 30.0 * step as f64,
                    y: 500.0 + 40.0 * f64::from(i),
                }),
            ));
        }
        ev.push((at(step * 16), None));
    }
    // All three lift.
    for slot in 0u32..3 {
        ev.push((at(176), Some(Event::Up { slot })));
    }
    ev.push((at(176), None));
    ev
}

#[test]
fn three_finger_swipe_arms_and_leaves_no_phantom_touches() {
    let mut r = replay(&three_finger_swipe(), Policy::default());

    // Patch 0011's regression, and the case that used to be impossible to
    // express: three bare fingers on a plain surface, mid-screen, with no
    // zone and no Mod. The first two go to the client, the third arms, and
    // the two client touches are cancelled on the spot.
    assert_eq!(
        r.ownership,
        vec![Ownership::Client, Ownership::Client, Ownership::Compositor],
        "arming is the only reason a mid-screen swipe works at all"
    );
    assert_eq!(
        r.cancelled,
        vec![vec![0, 1]],
        "exactly the two forwarded fingers are cancelled at the client"
    );
    assert_eq!(r.arb.open_client_touches(), 0, "no phantom is left open");

    // The fingers all lift, and the ledger ends empty and owing nothing.
    assert_eq!(r.arb.clear(), 0, "a trailing teardown owes nothing");
    assert_eq!(r.arb.ownership(0), None);
    assert_eq!(r.arb.ownership(2), None);

    // The control: the same gesture under a full-screen exclusion rect never
    // arms at all, because rect exclusion outranks the sequence and a rect
    // contact does not count toward arming.
    let rect_policy = Policy {
        exclusion_rects: vec![Rect {
            x: 0.0,
            y: 0.0,
            w: 1920.0,
            h: 1200.0,
        }],
        ..Policy::default()
    };
    let rected = replay(&three_finger_swipe(), rect_policy);
    assert!(
        rected.ownership.iter().all(|o| *o == Ownership::Client),
        "a full-screen exclusion rect means the client sees the whole swipe"
    );
    assert!(
        rected.cancelled.is_empty(),
        "and nothing was cancelled, because nothing was ever taken"
    );
    assert_eq!(rected.arb.open_client_touches(), 0);
}

#[test]
fn an_edge_swipe_is_never_forwarded_to_a_passthrough_app() {
    // A 3-finger swipe that starts in the left edge strip, under an app
    // matched by a passthrough window rule.
    let policy = Policy {
        passthrough_apps: vec!["org.kde.krita".to_string()],
        ..Policy::default()
    };
    let mut ev = Vec::new();
    ev.push((T0, Some(Event::UnderApp { passthrough: true })));
    for i in 0u32..3 {
        ev.push((
            T0,
            Some(Event::Down {
                slot: i,
                x: 20.0,
                y: 500.0 + 40.0 * f64::from(i),
            }),
        ));
    }
    ev.push((T0, None));
    for slot in 0u32..3 {
        ev.push((at(16), Some(Event::Up { slot })));
    }

    let mut r = replay(&ev, policy);
    assert_eq!(
        r.ownership,
        vec![
            Ownership::Compositor,
            Ownership::Compositor,
            Ownership::Compositor
        ],
        "a zone beats a passthrough app, so the app never sees a phantom"
    );
    assert_eq!(r.arb.open_client_touches(), 0);
    assert_eq!(r.arb.clear(), 0);
}

#[test]
fn the_same_swipe_mid_screen_is_the_passthrough_apps() {
    // The control for the test above: identical fingers, identical app,
    // but landed away from any zone. Now the window rule wins, and the app
    // is handed all three contacts — which is the behaviour niri-touch's
    // `touchscreen-gesture-passthrough` exists to provide.
    let policy = Policy {
        passthrough_apps: vec!["org.kde.krita".to_string()],
        ..Policy::default()
    };
    let mut ev = vec![(T0, Some(Event::UnderApp { passthrough: true }))];
    for i in 0u32..3 {
        ev.push((
            T0,
            Some(Event::Down {
                slot: i,
                x: 600.0,
                y: 500.0 + 40.0 * f64::from(i),
            }),
        ));
    }
    ev.push((T0, None));
    for slot in 0u32..3 {
        ev.push((at(16), Some(Event::Up { slot })));
    }

    let mut r = replay(&ev, policy);
    assert_eq!(
        r.ownership,
        vec![Ownership::Client, Ownership::Client, Ownership::Client],
        "away from a zone, a passthrough app keeps every finger"
    );
    assert_eq!(r.arb.open_client_touches(), 0);
    assert_eq!(r.arb.clear(), 0);
}

#[test]
fn out_of_order_lift_keeps_the_ledger_consistent() {
    let mut ev = three_finger_swipe();
    // Lift slot 2 before slot 0.
    ev.push((at(192), Some(Event::Up { slot: 2 })));
    ev.push((at(200), Some(Event::Up { slot: 0 })));
    ev.push((at(208), Some(Event::Up { slot: 1 })));

    let r = replay(&ev, Policy::default());
    assert_eq!(r.ownership.len(), 3);
    assert_eq!(r.arb.open_client_touches(), 0);
    // A late down on a fresh slot starts a fresh sequence, because the
    // earlier ones are gone.
    let mut arb = r.arb;
    assert_eq!(
        arb.down(5, &DownContext::default()),
        Ownership::Client,
        "an empty sequence has no compositor owner to inherit"
    );
}

#[test]
fn hotplug_mid_gesture_closes_exactly_the_client_touches() {
    // Two bare fingers down — both the client's — then a third in a zone.
    let mut ev = vec![
        (
            T0,
            Some(Event::Down {
                slot: 0,
                x: 600.0,
                y: 500.0,
            }),
        ),
        (
            T0,
            Some(Event::Down {
                slot: 1,
                x: 600.0,
                y: 540.0,
            }),
        ),
        (
            T0,
            Some(Event::Down {
                slot: 2,
                x: 10.0,
                y: 500.0,
            }),
        ),
        (T0, None),
    ];
    // Device yanked out mid-gesture.
    ev.push((at(80), Some(Event::DeviceGone)));
    ev.push((at(80), None));

    let mut r = replay(&ev, Policy::default());
    assert_eq!(
        r.ownership,
        vec![Ownership::Client, Ownership::Client, Ownership::Compositor]
    );
    // The zone finger took the sequence at its own down, so the two client
    // touches were already cancelled then. By the time the device goes, the
    // client owes nothing more.
    assert_eq!(r.cancelled, vec![vec![0, 1]]);
    assert_eq!(
        r.torn_down,
        vec![0],
        "device removal must not ask for a second cancel of touches the \
         client has already been told to drop"
    );
    assert_eq!(
        r.arb.open_client_touches(),
        0,
        "the invariant: nothing is left open after a grab installs"
    );
    assert_eq!(r.arb.clear(), 0, "a second teardown owes nothing");

    // And the case the count is actually about: a device yanked *before*
    // anything arms. Two bare fingers, no third. Both are the client's, and
    // the teardown owes exactly those two.
    let mut early = vec![
        (
            T0,
            Some(Event::Down {
                slot: 0,
                x: 600.0,
                y: 500.0,
            }),
        ),
        (
            T0,
            Some(Event::Down {
                slot: 1,
                x: 600.0,
                y: 540.0,
            }),
        ),
        (T0, None),
    ];
    early.push((at(40), Some(Event::DeviceGone)));
    let r = replay(&early, Policy::default());
    assert_eq!(r.ownership, vec![Ownership::Client, Ownership::Client]);
    assert!(
        r.cancelled.is_empty(),
        "nothing armed, so nothing was taken back"
    );
    assert_eq!(
        r.torn_down,
        vec![2],
        "exactly the two client-visible touches must be closed, and no more"
    );
    assert_eq!(r.arb.open_client_touches(), 0);
}

#[test]
fn ten_fingers_land_and_lift_clean() {
    let mut ev = Vec::new();
    for i in 0u32..10 {
        ev.push((
            T0,
            Some(Event::Down {
                slot: i,
                x: 960.0,
                y: 600.0 + 4.0 * f64::from(i),
            }),
        ));
    }
    ev.push((T0, None));
    // Lift in a scrambled order.
    for slot in [9u32, 3, 0, 7, 1, 5, 2, 8, 4, 6] {
        ev.push((at(16), Some(Event::Up { slot })));
    }
    ev.push((at(16), None));

    let r = replay(&ev, Policy::default());
    assert_eq!(r.ownership.len(), 10);
    // Ten fingers: the first two are the client's, the third arms, and the
    // remaining seven inherit the sequence.
    assert_eq!(
        r.ownership[..2],
        [Ownership::Client, Ownership::Client],
        "arming needs three fingers even when ten are coming"
    );
    assert!(
        r.ownership[2..].iter().all(|o| *o == Ownership::Compositor),
        "once armed the whole sequence is ours: {:?}",
        r.ownership
    );
    assert_eq!(
        r.cancelled,
        vec![vec![0, 1]],
        "one arming, and it cancelled exactly the two forwarded fingers"
    );
    assert_eq!(r.arb.open_client_touches(), 0);
}

#[test]
fn pen_down_mid_finger_gesture_does_not_inherit_or_disturb() {
    // Two fingers land bare; then the user grabs the stylus.
    let ev = vec![
        (
            T0,
            Some(Event::Down {
                slot: 0,
                x: 600.0,
                y: 500.0,
            }),
        ),
        (
            T0,
            Some(Event::Down {
                slot: 1,
                x: 600.0,
                y: 540.0,
            }),
        ),
        (T0, None),
    ];

    let mut arb = Arbitrator::new(Policy::default());
    let mut seq = Vec::new();
    for (time, event) in &ev {
        assert_eq!(*time, T0);
        if let Some(Event::Down { slot, x, y }) = event {
            seq.push(arb.down(
                *slot,
                &DownContext {
                    pos: Point::new(*x, *y),
                    ..DownContext::default()
                },
            ));
        }
    }
    assert_eq!(seq, vec![Ownership::Client, Ownership::Client]);

    // Stylus down: always the client's, and it does not arm anything.
    assert_eq!(
        arb.down(
            20,
            &DownContext {
                is_pen: true,
                pos: Point::new(700.0, 400.0),
                ..DownContext::default()
            }
        ),
        Ownership::Client
    );

    // The third finger is the one that arms: two fingers is still two, and
    // the stylus did not make it three. The resting stylus neither armed the
    // recogniser nor blocked the finger that did.
    assert_eq!(
        arb.down(2, &DownContext::default()),
        Ownership::Compositor,
        "the third finger arms, not the stylus"
    );
    let cancelled = arb.clear_clients();
    assert_eq!(
        cancelled,
        vec![0, 1, 20],
        "the two fingers and the stylus are all the client's to cancel"
    );

    // Mod + stylus does arm: the barrel button is the user's override.
    assert_eq!(
        arb.down(
            21,
            &DownContext {
                is_pen: true,
                mod_down: true,
                pos: Point::new(700.0, 400.0),
                ..DownContext::default()
            }
        ),
        Ownership::Compositor
    );
    assert_eq!(arb.open_client_touches(), 0);

    // Teardown owes the client nothing: everything was already cancelled at
    // the arming, and the only live slots are the compositor's.
    assert_eq!(arb.clear(), 0);
    assert_eq!(arb.open_client_touches(), 0);
}

#[test]
fn mod_from_the_start_is_the_compositor_for_every_finger() {
    let mut ev = Vec::new();
    ev.push((T0, Some(Event::Mod { down: true })));
    for i in 0u32..3 {
        ev.push((
            T0,
            Some(Event::Down {
                slot: i,
                x: 600.0,
                y: 500.0 + 40.0 * f64::from(i),
            }),
        ));
    }
    ev.push((T0, None));
    for slot in 0u32..3 {
        ev.push((at(16), Some(Event::Up { slot })));
    }

    let r = replay(&ev, Policy::default());
    assert_eq!(
        r.ownership,
        vec![
            Ownership::Compositor,
            Ownership::Compositor,
            Ownership::Compositor
        ]
    );
    assert_eq!(r.arb.open_client_touches(), 0);
}

#[test]
fn the_kill_switch_passes_everything_through() {
    let policy = Policy {
        disabled: true,
        passthrough_apps: vec!["everything".to_string()],
        min_gesture_fingers: DEFAULT_MIN_GESTURE_FINGERS,
        exclusion_rects: vec![Rect {
            x: 0.0,
            y: 0.0,
            w: 1920.0,
            h: 1200.0,
        }],
    };
    let mut ev = vec![(T0, Some(Event::Mod { down: true }))];
    for i in 0u32..4 {
        ev.push((
            T0,
            Some(Event::Down {
                slot: i,
                x: 10.0,
                y: 500.0 + 40.0 * f64::from(i),
            }),
        ));
    }
    ev.push((T0, None));

    let mut r = replay(&ev, policy);
    assert!(
        r.ownership.iter().all(|o| *o == Ownership::Client),
        "disabled must outrank Mod and zones alike: {:?}",
        r.ownership
    );
    assert_eq!(r.arb.open_client_touches(), 4);
    assert_eq!(r.arb.clear(), 4);
}

#[test]
fn trace_replay_is_deterministic_across_runs() {
    let a = replay(&three_finger_swipe(), Policy::default());
    let b = replay(&three_finger_swipe(), Policy::default());
    assert_eq!(a.ownership, b.ownership);
}

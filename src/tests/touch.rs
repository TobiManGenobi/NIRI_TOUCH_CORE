//! End-to-end tests for the touchscreen touch-arbitration stack.
//!
//! Every test here drives a real `Server` through the genuine
//! `process_input_event` path, exactly as `binds.rs` does for the keyboard.
//! The point of the module is that the six defects found by inspecting
//! `touch_arbitration_grab` were all invisible to `cargo test` because touch
//! could not be driven at all; each test below names the defect it prevents
//! and fails if that defect is reintroduced.

use std::time::Duration;

use niri_config::{Action, Config};
use wayland_client::protocol::wl_surface::WlSurface;

use crate::input::touch_arbitration_grab;
use crate::tests::client::{ClientId, TouchEvent};
use crate::tests::fixture::Fixture;

/// The output the fixture builds. Contacts are placed well inside it so no
/// finger lands in the 12px edge zone, which is what "plain surface" means
/// for the arbitration tests.
const OUTPUT: (u16, u16) = (1920, 1080);
const CENTER: (f64, f64) = (960.0, 540.0);

/// A swipe this long clears the default 100px commit gate while staying on
/// screen.
const SWIPE: f64 = 300.0;

/// Explicit times: these tests must not depend on the wall clock. Each step
/// advances by a fixed amount.
const STEP: Duration = Duration::from_millis(10);

/// Three fingers down on a plain surface, swiped up, lifted.
///
/// This is the headline case that has never worked: the first two contacts go
/// to the client, and only the third arms the compositor, at which point the
/// whole sequence has to be reconstructed and fed to the recogniser
/// retroactively.
pub fn set_up() -> (Fixture, ClientId, WlSurface) {
    set_up_with_arming(3)
}

/// Build a fixture whose compositor arms after `arming` fingers.
///
/// The bind parser refuses `fingers=` below 3, so a two-finger swipe cannot
/// be bound. Lowering `min-gesture-fingers` instead lets two fingers *arm*
/// the compositor while the only bind available is the three-finger one,
/// which makes "was the pen counted as a third finger?" answerable: a bind
/// that fires means the pen was counted, and silence means it was not.
pub fn set_up_with_arming(arming: u32) -> (Fixture, ClientId, WlSurface) {
    let mut config = Config::parse_mem(&format!(
        "
        binds {{
            TouchSwipe fingers=3 direction=\"up\" {{ close-window; }}
        }}

        input {{
            touchscreen {{
                gestures {{
                    min-gesture-fingers {arming}
                }}
            }}
        }}
        "
    ))
    .unwrap();

    // knuffel doesn't understand #[cfg(test)]...
    for bind in &mut config.binds.0 {
        bind.action = Action::TestAction;
    }

    let mut f = Fixture::with_config(config);
    f.add_output(1, OUTPUT);
    f.add_touch_device();

    let id = f.add_client();
    let window = f.client(id).create_window();
    let surface = window.surface.clone();
    window.commit();
    f.roundtrip(id);

    // Size the window to the whole output so the touch point is genuinely
    // over a client surface: the arbitration tests are about a plain surface,
    // and a client that is not under the finger would silently pass the
    // phantom-touch assertions for the wrong reason.
    let window = f.client(id).window(&surface);
    window.set_size(OUTPUT.0, OUTPUT.1);
    window.attach_new_buffer();
    window.ack_last_and_commit();
    f.double_roundtrip(id);

    // The ledgers are thread-local and outlive a single test, so a test that
    // ran earlier on this thread could leave arming tallies behind. Start
    // every test from a known-empty ledger.
    touch_arbitration_grab::reset_persistent();

    // Both ledgers are `thread_local!`, and `cargo test` runs many cases on
    // the same worker thread, so a fixture that did not clear them would
    // inherit the previous test's contacts. Prove the precondition rather
    // than trusting it: if this ever fails, every other assertion in this
    // module is measuring contamination.
    assert!(
        touch_arbitration_grab::live_contacts().is_empty(),
        "fixture started with a dirty live-contact ledger: {:?}",
        touch_arbitration_grab::live_contacts()
    );
    assert_eq!(
        touch_arbitration_grab::open_client_touches(),
        0,
        "fixture started with a dirty ownership ledger"
    );

    (f, id, surface)
}

/// A three-finger swipe up, as a real digitiser would report it: all three
/// contacts down, a burst of motion, then the lifts.
///
/// Returns the number of `test-action`s the gesture fired, so a caller can
/// assert on the delta rather than on an absolute.
fn three_finger_swipe_up(f: &mut Fixture) -> usize {
    let before = f.test_action_count();
    let mut t = Duration::ZERO;
    for i in 0..3 {
        // Spread the fingers horizontally so the recogniser sees a stable
        // cluster, and keep them far from every edge.
        let x = CENTER.0 + (i as f64 - 1.0) * 150.0;
        f.touch_down(i, x, CENTER.1, t);
        t += STEP;
    }
    f.touch_frame(t);

    // Move the whole cluster up past the commit gate.
    for step in 1..=10 {
        for i in 0..3 {
            let x = CENTER.0 + (i as f64 - 1.0) * 150.0;
            f.touch_motion(i, x, CENTER.1 - SWIPE * step as f64 / 10.0, t);
        }
        t += STEP;
        f.touch_frame(t);
    }

    for i in 0..3 {
        f.touch_up(i, t);
        t += STEP;
    }
    f.touch_frame(t);

    f.test_action_count() - before
}

/// Assert the seat carries no touch grab.
///
/// "Grab outlived its sequence" is the bug class this subsystem keeps
/// producing, and it is invisible from the ledgers alone: a leaked grab
/// silently re-owns every later sequence. Cheap enough to assert at the end
/// of every test.
fn assert_no_grab(f: &mut Fixture) {
    let grabbed = {
        let state = f.niri_state();
        state
            .niri
            .seat
            .get_touch()
            .map(|t| t.is_grabbed())
            .unwrap_or(false)
    };
    assert!(
        !grabbed,
        "a touch grab outlived its sequence: the seat is still grabbed, so \
         every later sequence on this seat would be owned by a dead grab"
    );
}

/// Every contact the compositor cancelled was also closed at the client.
///
/// The client is the only party that can tell the difference between "the
/// compositor decided this finger was never its own" and "the compositor left
/// a phantom touch on the browser", so this is read off the wire rather than
/// off the ledger.
fn client_up_slots(events: &[TouchEvent]) -> Vec<u32> {
    events
        .iter()
        .filter_map(|e| match e {
            TouchEvent::Up { slot } => Some(*slot),
            _ => None,
        })
        .collect()
}

#[test]
fn three_finger_swipe_arms_and_fires_the_bind() {
    let (mut f, id, surface) = set_up();

    let fired = three_finger_swipe_up(&mut f);

    f.roundtrip(id);

    // Assert WHAT THE RECOGNISER ACTUALLY SAW, not that an action ran.
    //
    // Two earlier versions of this test were unsound and the mutation matrix
    // is what proved it:
    //   1. Asserting only "the bind fired" against a single three-finger bind passed even with
    //      `feed_sequence` gutted so only one contact reached the recogniser.
    //   2. Bracketing with a second `fingers=4` bind fixed the mutation, but the failure came from
    //      the invariant checker's debug_assert, not from this test's own assertion.
    //
    // `last_recognised_gesture` is recorded from the recogniser's own output
    // before any bind lookup, so it is the authoritative finger count. This
    // is the assertion that fails for this test's own reason.
    let gesture = touch_arbitration_grab::last_recognised_gesture()
        .expect("the three-finger swipe must have been recognised");
    assert_eq!(
        gesture.fingers, 3,
        "the recogniser must have seen three fingers, saw {} ({})",
        gesture.fingers, gesture.kind
    );
    assert_eq!(
        gesture.kind, "swipe",
        "the gesture must be a swipe, got {}",
        gesture.kind
    );

    // The bind must still fire; the seam and the action are independent
    // claims and both should hold.
    assert_eq!(
        fired, 1,
        "the three-finger upward swipe must fire its bind exactly once"
    );

    // Nothing may have been left behind: the client was closed out and the
    // ledgers drained.
    assert_eq!(touch_arbitration_grab::open_client_touches(), 0);
    assert!(touch_arbitration_grab::live_contacts().is_empty());

    let _ = surface;

    assert_no_grab(&mut f);
}

/// A contact cancelled at the client must be closed there too.
///
/// When the third finger arms the compositor, the two contacts already
/// forwarded to the client are closed with a touch-end. Leaving them open is
/// the phantom-touch bug: the browser keeps a finger down, pinch-zooms, and
/// then ignores every later tap.
#[test]
fn no_client_contact_survives_arming() {
    let (mut f, id, surface) = set_up();

    let mut t = Duration::ZERO;
    let mut sent = Vec::new();
    for i in 0..3 {
        let x = CENTER.0 + (i as f64 - 1.0) * 150.0;
        f.touch_down(i, x, CENTER.1, t);
        sent.push(i);
        t += STEP;
    }
    f.touch_frame(t);
    f.roundtrip(id);

    // The first two contacts went to the client.
    // NOTE: this drain happens *after* the arming down, so it also contains
    // the touch-ends the compositor sent when it took the sequence over.
    // Draining again later and expecting to find them there would be a bug in
    // the test, not in the compositor.
    let before_arming: Vec<_> = f.client(id).state.recent_touch_events(&surface).collect();
    let downs_before: Vec<u32> = before_arming
        .iter()
        .filter_map(|e| match e {
            TouchEvent::Down { slot, .. } => Some(*slot),
            _ => None,
        })
        .collect();
    let ups_before = client_up_slots(&before_arming);
    assert_eq!(
        downs_before,
        vec![0, 1],
        "the first two fingers belong to the client until the third arms"
    );

    // The third finger arms: the two client contacts are cancelled.
    let before = f.test_action_count();
    for step in 1..=10 {
        for i in 0..3 {
            let x = CENTER.0 + (i as f64 - 1.0) * 150.0;
            f.touch_motion(i, x, CENTER.1 - SWIPE * step as f64 / 10.0, t);
        }
        t += STEP;
        f.touch_frame(t);
    }
    for i in 0..3 {
        f.touch_up(i, t);
        t += STEP;
    }
    f.touch_frame(t);
    f.roundtrip(id);
    assert_eq!(f.test_action_count() - before, 1);

    // Every contact that was cancelled at the client must have been closed
    // there with a touch-end.
    let ups = ups_before;

    for cancelled in &downs_before {
        assert!(
            ups.contains(cancelled),
            "contact {cancelled} was cancelled by the compositor but never \
             received a touch-end; the client would believe it is still down \
             (client saw ups for {ups:?})"
        );
    }

    let _ = sent;

    assert_no_grab(&mut f);
}

/// Neither ledger may outlive the sequence.
///
/// A contact that lifts without being retired stays in the ledger forever,
/// which both inflates the arming tally for the next gesture and mis-feeds
/// `feed_sequence`.
#[test]
fn ledger_is_empty_after_every_sequence() {
    let (mut f, _id, _surface) = set_up();

    let fired = three_finger_swipe_up(&mut f);

    // Without this the test is vacuous: if nothing ever armed, the ledgers
    // would trivially be empty. Prove a real gesture ran in this same test,
    // so "empty afterwards" means "cleaned up", not "never used".
    assert_eq!(
        fired, 1,
        "a gesture must have armed and fired in this test, otherwise the \
         ledger assertions below prove nothing"
    );

    assert_eq!(
        touch_arbitration_grab::open_client_touches(),
        0,
        "the ownership ledger must be empty once every contact has lifted"
    );
    assert!(
        touch_arbitration_grab::live_contacts().is_empty(),
        "the live-contact ledger must be empty once every contact has lifted, \
         got {:?}",
        touch_arbitration_grab::live_contacts()
    );

    assert_no_grab(&mut f);
}

/// A second gesture on the same seat must work exactly like the first.
///
/// This is the composite check: every one of the ledger defects ultimately
/// broke the *next* gesture rather than the current one, because a stale entry
/// only matters once a fresh sequence queries the tally.
#[test]
fn a_second_gesture_after_a_full_sequence_is_not_poisoned() {
    let (mut f, id, surface) = set_up();

    let first = three_finger_swipe_up(&mut f);
    f.roundtrip(id);
    let _ = f.client(id).state.recent_touch_events(&surface);

    let second = three_finger_swipe_up(&mut f);
    f.roundtrip(id);

    assert_eq!(first, 1, "the first gesture must fire");
    assert_eq!(
        second, 1,
        "the second gesture must fire too; a stale ledger entry from the \
         first sequence poisoned it"
    );

    assert_eq!(touch_arbitration_grab::open_client_touches(), 0);
    assert!(touch_arbitration_grab::live_contacts().is_empty());

    assert_no_grab(&mut f);
}

/// Cancelling a client contact must not retire it from the live ledger.
///
/// `handle.up` during stranding re-enters the compositor's own `up` handler.
/// If that path treated the synthetic close as the finger leaving the glass,
/// the contact would be retired while still physically down, and the
/// recogniser would never see it again — the gesture would complete with the
/// wrong finger count, or not at all.
#[test]
fn stranding_does_not_freeze_the_earlier_contacts() {
    let (mut f, _id, _surface) = set_up();

    let fired = three_finger_swipe_up(&mut f);

    // The bind is three fingers. If either stranded contact had been retired
    // by the synthetic close, the recogniser would have seen fewer than three
    // fingers and never crossed `min_fingers`, so nothing would have fired.
    assert_eq!(
        fired, 1,
        "the earlier contacts must keep receiving motion and their lift, so \
         the gesture completes as a three-finger swipe"
    );

    assert_no_grab(&mut f);
}

/// A resting stylus must not be counted as a finger.
///
/// The pen is refused by the arbitrator, so it must be refused by the
/// recogniser too. Feeding it would make the gesture report three fingers
/// when it was made of two, and would drag its centroid toward the pen.
#[test]
fn a_resting_pen_does_not_become_a_third_finger() {
    // Arm at three so every contact is on the glass before the compositor
    // takes the sequence. The only bind is the three-finger one, so whether
    // it fires is a direct readout of the recogniser's finger count: with the
    // pen miscounted the gesture is three fingers and fires, correctly
    // counted it is two and never reaches the gate.
    let (mut f, id, surface) = set_up_with_arming(2);

    // A stylus resting on the glass, held up as a finger would be.
    f.set_pen(true);

    let mut t = Duration::ZERO;
    f.touch_down(0, CENTER.0, CENTER.1, t);
    t += STEP;

    // The tool is a property of the DIGITISER, not of a contact, so put the
    // finger contacts on a second digitiser that is not a pen. Without this
    // the two 'fingers' are also pens and the test asserts nothing.
    f.set_touch_device_id(2);
    f.set_pen(false);

    // Two real fingers. With the pen miscounted this is a three-finger
    // swipe; correctly counted it is a two-finger swipe.
    for i in 1..3 {
        let x = CENTER.0 + (i as f64 - 1.0) * 150.0;
        f.touch_down(i, x, CENTER.1, t);
        t += STEP;
    }
    f.touch_frame(t);
    f.roundtrip(id);

    // Assert the PREDICATE the mutation flips, not a downstream bind.
    //
    // An earlier version inferred the answer from whether the three-finger
    // bind fired, and a mutation that makes `counts_as_gesture_input` always
    // true left it green: the gesture never reached the bind either way, so
    // the assertion had no purchase. The pen's eligibility is directly
    // readable from the live ledger, so assert that instead.
    let contacts = touch_arbitration_grab::live_contacts();
    let pen = contacts
        .iter()
        .find(|c| c.is_pen)
        .expect("the pen contact must be on the live ledger");
    assert!(
        !pen.counts_as_gesture_input(),
        "a resting pen must not count as gesture input: it would become a \
         third finger the recogniser never agreed to"
    );

    // The two fingers DO count, so the sequence can still arm. This makes the
    // assertion above meaningful rather than vacuous.
    let fingers: Vec<_> = contacts.iter().filter(|c| !c.is_pen).collect();
    assert_eq!(fingers.len(), 2, "both fingers must be on the live ledger");
    for finger in &fingers {
        assert!(
            finger.counts_as_gesture_input(),
            "a finger must count as gesture input"
        );
    }

    // Drive the gesture to completion so the ledgers are drained, and prove
    // the compositor really did arm (no phantom fingers left behind).
    for step in 1..=10 {
        for i in 0..3 {
            let x = CENTER.0 + (i as f64 - 1.0) * 150.0;
            f.touch_motion(i, x, CENTER.1 - SWIPE * step as f64 / 10.0, t);
        }
        t += STEP;
        f.touch_frame(t);
    }
    for i in 0..3 {
        f.touch_up(i, t);
        t += STEP;
    }
    f.touch_frame(t);
    f.roundtrip(id);

    let ups = client_up_slots(
        &f.client(id)
            .state
            .recent_touch_events(&surface)
            .collect::<Vec<_>>(),
    );
    assert!(
        !ups.is_empty(),
        "the two fingers must have armed the compositor; without arming this \
         test would pass for the wrong reason"
    );

    assert_eq!(touch_arbitration_grab::open_client_touches(), 0);
    assert!(touch_arbitration_grab::live_contacts().is_empty());

    assert_no_grab(&mut f);
}

/// Mod is the escape hatch: a pen under Mod is a deliberate gesture input.
///
/// Unlike a resting pen, it must be fed to the recogniser.
#[test]
fn mod_plus_pen_arms_the_compositor() {
    let (mut f, id, surface) = set_up();

    f.set_pen(true);

    // Control: with no Mod, the resting pen belongs to the client.
    f.touch_down(0, CENTER.0, CENTER.1, Duration::ZERO);
    f.touch_frame(STEP);
    f.roundtrip(id);
    let client_saw_pen: Vec<_> = f.client(id).state.recent_touch_events(&surface).collect();
    assert!(
        client_saw_pen
            .iter()
            .any(|e| matches!(e, TouchEvent::Down { slot: 0, .. })),
        "without Mod the resting pen belongs to the client"
    );
    f.touch_up(0, STEP);
    f.touch_frame(STEP * 2);

    // Now the same pen under Mod. Mod is the escape hatch: a pen under Mod is
    // a deliberate gesture input, so the compositor must take it.
    f.mod_down();
    let mut t = STEP * 3;
    f.touch_down(0, CENTER.0, CENTER.1, t);
    t += STEP;
    f.touch_frame(t);

    // Second finger joins, so the recogniser has something to work with.
    f.touch_down(1, CENTER.0 + 150.0, CENTER.1, t);
    t += STEP;
    f.touch_frame(t);

    for step in 1..=10 {
        f.touch_motion(0, CENTER.0, CENTER.1 - SWIPE * step as f64 / 10.0, t);
        f.touch_motion(
            1,
            CENTER.0 + 150.0,
            CENTER.1 - SWIPE * step as f64 / 10.0,
            t,
        );
        t += STEP;
        f.touch_frame(t);
    }
    f.touch_up(0, t);
    t += STEP;
    f.touch_up(1, t);
    f.touch_frame(t);
    f.mod_up();
    f.roundtrip(id);

    // The observable: under Mod the compositor owns the pen, so the client
    // never sees a down for it. A pen fed to the client instead would mean
    // Mod did not take the sequence.
    let after: Vec<_> = f.client(id).state.recent_touch_events(&surface).collect();
    assert!(
        !after
            .iter()
            .any(|e| matches!(e, TouchEvent::Down { slot: 0, .. })),
        "under Mod the pen must be claimed by the compositor, not forwarded \
         to the client"
    );

    assert_eq!(touch_arbitration_grab::open_client_touches(), 0);
    assert!(
        touch_arbitration_grab::live_contacts().is_empty(),
        "a Mod+pen gesture must leave both ledgers empty"
    );

    assert_no_grab(&mut f);
}

/// An eraser tip is not a finger, exactly as a bare pen is not.
///
/// The arbitrator refuses an eraser *before* the Mod escape hatch, so even an
/// eraser under Mod stays out of the recogniser. This is the sibling of
/// `a_resting_pen_does_not_become_a_third_finger` and pins the other half of
/// `LiveContact::counts_as_gesture_input`.
#[test]
fn an_eraser_under_mod_is_not_counted_as_a_finger() {
    // `_surface`: this test asserts on the ledgers, not on the client's view.
    // Under Mod every contact here is compositor-owned, so there is no
    // client touch event to assert on — see the note before the `contacts`
    // assertion below.
    let (mut f, id, _surface) = set_up_with_arming(2);

    // An eraser tip, with Mod held. Mod feeds a pen to the recogniser; it must
    // not rescue an eraser.
    f.set_eraser(true);
    f.mod_down();

    let mut t = Duration::ZERO;
    f.touch_down(0, CENTER.0, CENTER.1, t);
    t += STEP;

    // The tool belongs to the digitiser, not the contact, so the two fingers
    // go on a second digitiser that is an ordinary fingertip.
    f.set_touch_device_id(2);
    f.set_eraser(false);

    for i in 1..3 {
        let x = CENTER.0 + (i as f64 - 1.0) * 150.0;
        f.touch_down(i, x, CENTER.1, t);
        t += STEP;
    }
    f.touch_frame(t);
    f.roundtrip(id);

    // Assert the predicate directly. Inferring from bind firing left this
    // test green under a mutation of `counts_as_gesture_input`, because the
    // gesture never reached a bind either way.
    let contacts = touch_arbitration_grab::live_contacts();

    // Prove the sequence armed, so the eraser assertions below are not
    // trivially true because nothing happened.
    //
    // NOT via client touch-ends: under Mod the ERASER arms the compositor by
    // itself (step 4 of `decide`), so the two fingers are compositor-owned
    // from the start and never forwarded to the client — there are no client
    // ups to find. The evidence is instead that all three contacts reached the
    // live ledger and the eraser records Mod as held, which is exactly what
    // makes the exclusion below a real test of the Mod escape hatch.
    assert_eq!(
        contacts.len(),
        3,
        "all three contacts must be on the live ledger: {contacts:?}"
    );

    let eraser = contacts
        .iter()
        .find(|c| c.is_eraser)
        .expect("the eraser contact must be on the live ledger");
    assert!(
        !eraser.counts_as_gesture_input(),
        "an eraser tip must not count as gesture input, not even under Mod: \
         step 3 of `decide` places the eraser BEFORE the Mod escape hatch"
    );

    // Mod must still be recorded on the contact, or the assertion above would
    // pass for the wrong reason (an eraser with no Mod would be trivially
    // excluded).
    assert!(
        eraser.mod_down,
        "the eraser contact must record that Mod was held, otherwise this \
         test is not actually exercising the Mod escape hatch"
    );

    let fingers: Vec<_> = contacts.iter().filter(|c| !c.is_eraser).collect();
    assert_eq!(fingers.len(), 2, "both fingers must be on the live ledger");
    for finger in &fingers {
        assert!(
            finger.counts_as_gesture_input(),
            "a finger must count as gesture input"
        );
    }

    for step in 1..=10 {
        for i in 0..3 {
            let x = CENTER.0 + (i as f64 - 1.0) * 150.0;
            f.touch_motion(i, x, CENTER.1 - SWIPE * step as f64 / 10.0, t);
        }
        t += STEP;
        f.touch_frame(t);
    }

    for i in 0..3 {
        f.touch_up(i, t);
        t += STEP;
    }
    f.touch_frame(t);
    f.mod_up();

    assert_eq!(touch_arbitration_grab::open_client_touches(), 0);
    assert!(touch_arbitration_grab::live_contacts().is_empty());

    assert_no_grab(&mut f);
}

/// A cancel ends every contact at once and names none of them.
///
/// The ledgers therefore have to be cleared wholesale, or they keep an arming
/// tally for contacts that no longer exist.
#[test]
fn touch_cancel_clears_every_ledger() {
    let (mut f, _id, _surface) = set_up();

    let mut t = Duration::ZERO;
    for i in 0..3 {
        let x = CENTER.0 + (i as f64 - 1.0) * 150.0;
        f.touch_down(i, x, CENTER.1, t);
        t += STEP;
    }
    f.touch_frame(t);
    for step in 1..=10 {
        for i in 0..3 {
            let x = CENTER.0 + (i as f64 - 1.0) * 150.0;
            f.touch_motion(i, x, CENTER.1 - SWIPE * step as f64 / 10.0, t);
        }
        t += STEP;
        f.touch_frame(t);
    }

    // The gesture is live; cancel mid-flight.
    f.touch_cancel(t);

    assert_eq!(
        touch_arbitration_grab::open_client_touches(),
        0,
        "a cancel must clear the ownership ledger"
    );
    assert!(
        touch_arbitration_grab::live_contacts().is_empty(),
        "a cancel must clear the live-contact ledger, got {:?}",
        touch_arbitration_grab::live_contacts()
    );

    assert_no_grab(&mut f);
}

/// A device yanked mid-sequence must not leave the grab disagreeing with the
/// ledgers.
///
/// This is the one path that used to clear the persistent ledgers with the
/// grab still installed. The seat keeps its touch capability whenever another
/// digitiser remains, so the live grab kept its `fed_slots`,
/// `compositor_slots` and `client_slots` while `TOUCH_CONTACTS` went empty —
/// and the next motion ran `check_invariants` against that disagreement and
/// panicked. Measured against the pre-fix code, the assertion that trips is
/// "fed slots routed or retro-fed and still down": `up` empties the routing
/// sets, and nothing ever retires the fed set, so the fed slots are the first
/// thing the invariant finds.
///
/// The panic is the real assertion here — `motion` is the call that trips it,
/// so a regression fails the test rather than quietly passing it. The
/// post-conditions are asserted first as well, because "did not panic" alone
/// would also be satisfied by a fix that tore down the seat's whole touch
/// capability instead of the sequence.
///
/// `end_sequence_on_device_loss` is called directly rather than through
/// `process_libinput_event`: the fixture drives `TestInputBackend`, and
/// `process_libinput_event` is `impl InputHandler<LibinputInputBackend>`, so
/// the raw branch is unreachable from here. The device-removal branch calls
/// exactly this function and nothing else, and the test proves the function's
/// contract — which is where the bug lived.
#[test]
fn a_device_removed_mid_sequence_leaves_no_routed_slot_behind() {
    let (mut f, id, _surface) = set_up();

    let mut t = Duration::ZERO;
    for i in 0..3 {
        let x = CENTER.0 + (i as f64 - 1.0) * 150.0;
        f.touch_down(i, x, CENTER.1, t);
        t += STEP;
    }
    f.touch_frame(t);
    for step in 1..=5 {
        for i in 0..3 {
            let x = CENTER.0 + (i as f64 - 1.0) * 150.0;
            f.touch_motion(i, x, CENTER.1 - SWIPE * step as f64 / 5.0, t);
        }
        t += STEP;
        f.touch_frame(t);
    }

    // Precondition: the sequence really is live and really is grabbed, so a
    // pass below cannot come from there having been nothing to tear down.
    assert_eq!(touch_arbitration_grab::live_contacts().len(), 3);
    {
        let state = f.niri_state();
        assert!(
            state.niri.seat.get_touch().map(|t| t.is_grabbed()) == Some(true),
            "the sequence must be live and grabbed when the device goes away"
        );
    }

    // The device goes away mid-sequence. The seat keeps its touch capability,
    // exactly as it would with a second digitiser still attached.
    touch_arbitration_grab::end_sequence_on_device_loss(f.niri_state());

    // The sequence is gone: ledgers empty, grab uninstalled, and the seat
    // still offers wl_touch so a surviving device keeps working.
    assert_eq!(
        touch_arbitration_grab::open_client_touches(),
        0,
        "device removal must clear the ownership ledger"
    );
    assert!(
        touch_arbitration_grab::live_contacts().is_empty(),
        "device removal must clear the live-contact ledger, got {:?}",
        touch_arbitration_grab::live_contacts()
    );
    assert_no_grab(&mut f);
    assert!(
        f.niri_state().niri.seat.get_touch().is_some(),
        "the seat must keep its touch capability while a digitiser remains"
    );

    // The regression itself. A surviving digitiser reports fresh motion, and
    // the grab — if it had outlived its sequence — would run
    // `check_invariants` here and panic.
    for step in 6..=10 {
        for i in 0..3 {
            let x = CENTER.0 + (i as f64 - 1.0) * 150.0;
            f.touch_motion(i, x, CENTER.1 - SWIPE * step as f64 / 10.0, t);
        }
        t += STEP;
        f.touch_frame(t);
    }
    f.roundtrip(id);

    // And the ledgers are still consistent with the grab after it — a fix
    // that stopped the panic by clearing on every motion would show up here.
    assert!(
        touch_arbitration_grab::live_contacts().is_empty(),
        "motion for slots the seat was never told about must not repopulate \
         the ledger, got {:?}",
        touch_arbitration_grab::live_contacts()
    );

    assert_no_grab(&mut f);
}

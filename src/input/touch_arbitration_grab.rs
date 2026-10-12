//! L3: the compositor adapter that decides who owns each touch.
//!
//! This file is deliberately thin. Contact geometry and gesture recognition
//! live in `touch-core` (pure, no compositor types); the ownership decision
//! lives in `touch-arbitrate` (pure, no compositor types). What is left here
//! is the part that cannot be tested without a live seat: translating niri's
//! notion of "where did this finger land" into a [`DownContext`], installing
//! and uninstalling the grab at the right moments, and routing each event to
//! either the recogniser or the client.
//!
//! Keeping this file small is the whole point. The two forks this replaces
//! both grew past a thousand lines by fusing the tracker, the recogniser and
//! the seat bookkeeping into one `enum State`; the bugs that needed twenty
//! patches to paper over were almost all bugs in that fused state machine,
//! and almost none of them were bugs in the geometry.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use niri_config::{
    Action, Config, EdgeZone, Modifiers, PinchDirection, RotateDirection, ScreenEdge,
    SwipeDirection, Trigger,
};
use niri_ipc::TouchOwnership;
use smithay::backend::input::{Device as _, TouchSlot};
use smithay::backend::winit::WinitVirtualDevice;
use smithay::input::keyboard::ModifiersState;
use smithay::input::touch::{
    DownEvent, GrabStartData as TouchGrabStartData, MotionEvent, OrientationEvent, ShapeEvent,
    TouchGrab, TouchInnerHandle, UpEvent,
};
use smithay::input::SeatHandler;
use smithay::output::Output;
use smithay::utils::{Logical, Point, SERIAL_COUNTER};
use touch_arbitrate::{Arbitrator, DownContext, Ownership, Policy, Rect};
use touch_core::{GestureEvent, OutputSize, Point as CorePoint, Recognizer, RecognizerConfig};
use tracing::{debug, warn};

use crate::input::AnyStartData;
use crate::layout::LayoutElement as _;
use crate::niri::State;
use crate::protocols::virtual_pointer::VirtualPointer;

/// Device identity as the arbitrator sees it.
///
/// The arbitrator's `DownContext` wants three things from the hardware — a
/// stable numeric id, whether the contact is a stylus, and whether that
/// stylus is an eraser — but smithay's `Device` trait exposes none of them,
/// so they are pulled out here for the backends that can answer. Devices that
/// cannot answer report a finger, which is the safe default: a finger is
/// eligible for compositor gestures, an unknown tool should not be
/// misclassified as one.
pub trait TouchDeviceInfo {
    /// The device this contact came from, as the arbitrator's
    /// `DownContext::device_id`.
    ///
    /// The value is [`hash_device_id`] of the backend's `Device::id()`
    /// string — for libinput, the device sysname. `DefaultHasher` is SipHash
    /// with fixed keys rather than randomly seeded, so the hash is
    /// **deterministic across runs**, which is the point: a user reporting a
    /// multi-digitizer bug can paste the number and it means the same thing on
    /// their machine as on ours. It is stable for as long as the device node
    /// exists, including across a re-plug (libinput builds a new `Device`, but
    /// the sysname is the same string), and two digitizers get genuinely
    /// different values because they have different sysnames. As a device
    /// discriminator the value is sound.
    ///
    /// # It is read nowhere, and that is deliberate
    ///
    /// Do not read the presence of this method as evidence that contacts are
    /// device-scoped. They are not, and no change confined to this file could
    /// make them so.
    ///
    /// Every slot-keyed ledger in the tree is keyed on the bare slot number
    /// with no device component: `Arbitrator::slots` (`BTreeMap<u32, _>`) and
    /// `TOUCH_ARBITRATOR`, `LiveContact::slot` and `TOUCH_CONTACTS`,
    /// `client_slots`, `compositor_slots`, `fed_slots` — see the key-space
    /// discussion on [`slot_id`]. More decisively, so is smithay's
    /// `TouchInternal.focus`, and that one is not ours to change:
    ///
    /// ```text
    /// pub(crate) focus: HashMap<TouchSlot, TouchSlotState<D>>,
    /// ```
    ///
    /// (smithay `src/input/touch/mod.rs`, rev `19c16d3`.) One map per
    /// *seat*, keyed on bare `TouchSlot`. There is no device component on
    /// the delivery path at all: `TouchHandle::down` (`:334`) ->
    /// `TouchInnerHandle::down` (`:482`) -> `TouchInternal::down` (`:577`)
    /// -> `DefaultGrab::down` (`src/input/touch/grab.rs:145`) each take
    /// only `(&mut D, Option<(TouchFocus, Point<f64, Logical>)>,
    /// &DownEvent)` — no `device` parameter exists on any of them
    /// (`handle_touch_down` appears nowhere in the tree). Only `event.slot`
    /// reaches `self.focus.entry(...)`, and the routing target is then read
    /// back out of that single entry via `self.focus.get(&event.slot)`. So
    /// two digitizers both pressing slot 0 alias **inside smithay, in the
    /// routing target itself**: the second `entry` call overwrites the
    /// first contact's `TouchFocus` with `clone_from`, and the first
    /// device's `up` then routes to the second device's client surface.
    /// That happens before any niri code runs. The aliasing is
    /// structural — there is no device argument to discard — and so no
    /// change confined to this file could address it.
    ///
    /// # Why re-keying our ledgers by `(device_id, slot)` was rejected
    ///
    /// Because it fixes nothing and costs the invariant. Our ledger would
    /// correctly hold two entries while the seat had already merged the two
    /// contacts into one focus target and delivered one device's `down` to
    /// the other device's client surface. The ledger would claim contacts are
    /// distinct that the seat has already merged, deleting the only visible
    /// trace of the problem.
    ///
    /// It also breaks the load-bearing one. `clear_clients` returns the slots
    /// whose client touches the adapter must close, and that is correct only
    /// because our key *is* the seat's key — which is exactly why invariant 5
    /// can assert that the grab's arbitrator agrees with `client_slots` in
    /// count and slot by slot. Under a composite key the arbitrator could
    /// return a slot the seat never opened a touch for, and the adapter would
    /// close a phantom point or leave a real one dangling. A visible bug
    /// becomes a silent one, which is strictly worse.
    ///
    /// # Where the fix belongs
    ///
    /// Upstream, in two places: device-scoped slot identity in smithay's touch
    /// focus, and matching keying in `touch-arbitrate`'s `Arbitrator`, which
    /// has the identical defect. Until that lands, this id is the context that
    /// fix will need — it is the only record of *which* device a contact came
    /// from, and without it a bug report carries nothing that could diagnose
    /// it. Deleting it now would mean re-plumbing three backend impls at the
    /// moment the information becomes most valuable.
    ///
    /// # Practical consequence
    ///
    /// On a machine with both a touchscreen and a stylus, pen arbitration can
    /// misbehave in ways that are **not** a bug in this layer — the two
    /// devices may simply be sharing slot 0. Check that first before
    /// investigating the arbitration code.
    fn touch_device_id(&self) -> u32;

    /// The contact comes from a stylus rather than a finger.
    fn touch_is_pen(&self) -> bool;

    /// The contact is an eraser tip.
    fn touch_is_eraser(&self) -> bool;

    /// All three fields at once, as the arbitrator's `DownContext` wants them.
    fn touch_device_info(&self) -> (u32, bool, bool) {
        (
            self.touch_device_id(),
            self.touch_is_pen(),
            self.touch_is_eraser(),
        )
    }
}

/// Stable hash of the backend's device id string.
fn hash_device_id(id: &str) -> u32 {
    let mut hasher = DefaultHasher::new();
    id.hash(&mut hasher);
    hasher.finish() as u32
}

impl TouchDeviceInfo for input::Device {
    fn touch_device_id(&self) -> u32 {
        hash_device_id(&self.id())
    }

    // libinput reports the tool through udev rather than the touch event, so
    // this is a property of the digitiser rather than of any one contact.
    fn touch_is_pen(&self) -> bool {
        self.has_touch_tool("ID_INPUT_TOOL_PEN")
    }

    fn touch_is_eraser(&self) -> bool {
        self.has_touch_tool("ID_INPUT_TOOL_ERASER")
    }
}

impl TouchDeviceInfo for WinitVirtualDevice {
    fn touch_device_id(&self) -> u32 {
        hash_device_id(&self.id())
    }
    fn touch_is_pen(&self) -> bool {
        false
    }
    fn touch_is_eraser(&self) -> bool {
        false
    }
}

impl TouchDeviceInfo for VirtualPointer {
    fn touch_device_id(&self) -> u32 {
        hash_device_id(&self.id())
    }
    fn touch_is_pen(&self) -> bool {
        false
    }
    fn touch_is_eraser(&self) -> bool {
        false
    }
}

/// Helper for the libinput impl above.
trait TouchToolProperty {
    fn has_touch_tool(&self, property: &str) -> bool;
}

impl TouchToolProperty for input::Device {
    fn has_touch_tool(&self, property: &str) -> bool {
        // Safety: the udev context outlives the borrow, which is why libinput
        // refs it for us. niri already does this same call in
        // `apply_libinput_settings`, so the safety argument is not new.
        match unsafe { self.udev_device() } {
            Some(udev) => udev.property_value(property).is_some(),
            None => false,
        }
    }
}

/// Mark the start of a new touch sequence.
///
/// This is the ONE place the recognised-gesture seam is cleared, and it is
/// deliberately on ENTRY rather than on exit. See the note on
/// [`reset_persistent`]: that function runs at end-of-sequence, after the
/// recogniser has dispatched the gesture, so clearing there would erase the
/// value before anyone could read it.
///
/// Clearing on entry is what keeps the value attributable. A caller that
/// performs a gesture and then reads the seam gets that gesture; a caller
/// that has not started a sequence — a fresh fixture, for instance — has
/// recognised nothing and reads `None` rather than a previous gesture's
/// value.
pub fn begin_sequence() {
    LAST_RECOGNISED_GESTURE.with(|last| last.set(None));
}

/// libinput slot ids, as the `u32` the pure crates key their ledgers by.
///
/// A device that reports no slot id (some single-contact touchscreens do this)
/// maps to 0, because `i32::from(TouchSlot)` yields -1 only for the `None`
/// case. Note that `Some(0)` *also* maps to 0. Within a single digitiser that
/// is harmless: a device with no slot ids can only have one contact down, so
/// nothing can collide with it.
///
/// # KNOWN LIMITATION: this is not device-scoped, and cannot be here
///
/// Across devices the collision is real. Everything this function keys —
/// `TOUCH_ARBITRATOR`, `TOUCH_CONTACTS`, `client_slots`, `compositor_slots`,
/// `fed_slots`, and the `u32` key of `touch-arbitrate`'s `Arbitrator` — is a
/// global keyed by slot number alone. Two digitisers both using slot 0 alias
/// onto one entry: `note_live_contact`'s `retain(|c| c.slot != slot)` silently
/// evicts the other device's contact, and `clear_clients` can hand back a slot
/// belonging to a different digitiser, which the adapter then closes at the
/// wrong client.
///
/// The collision starts one layer below us, though, and that is what makes
/// this unfixable from here. smithay's own `TouchInternal.focus` is a
/// `HashMap<TouchSlot, TouchSlotState<D>>` — keyed by bare `TouchSlot`,
/// globally, not per device. A pen and a touchscreen sharing one seat
/// therefore collide *inside smithay*, before any niri code runs, and the
/// seat's touch focus — the routing target of every `handle.down` and
/// `handle.up` — cannot tell them apart.
///
/// `DownContext::device_id` is carried for future use but currently scopes
/// nothing at all. It is read nowhere in the tree. Its presence is NOT
/// evidence that contacts are device-scoped; do not read it that way.
///
/// An adapter-local workaround was considered and deliberately rejected:
/// re-keying our ledgers by `(device_id, slot)` while smithay still conflates
/// the two contacts would make our bookkeeping claim they are distinct while
/// the seat merges them. That converts a visible bug into a silent one, which
/// is strictly worse than the visible one.
///
/// The fix belongs upstream, in two places: device-scoped slot identity in
/// smithay's touch focus, and matching keying in `touch-arbitrate`'s
/// `Arbitrator`, which is also keyed on a bare `u32`.
///
/// # Operational consequence
///
/// On a setup with both a touchscreen and a stylus, pen behaviour can misbehave
/// in ways that are **not** a bug in the arbitration logic. If a pen-related
/// test or a pen gesture fails on such a machine, check this first before
/// investigating the arbitration code — the two devices may simply be sharing
/// slot 0.
pub fn slot_id(slot: TouchSlot) -> u32 {
    match i32::from(slot) {
        id if id >= 0 => id as u32,
        _ => 0,
    }
}

/// The name of the precedence step that produced a ruling.
///
/// # Exactness
///
/// This mirrors `touch_arbitrate::decide` step for step, including the
/// conditions each step tests, so it cannot report a step that `decide` did
/// not take. The crate returns only an `Ownership` and cannot be asked which
/// rule fired, so the alternative is duplicating the chain and hoping the two
/// copies stay in step — and a log that names the wrong step is worse than no
/// log at all, because it sends the reader to the wrong line of the contract.
///
/// **If `decide` ever changes its precedence, this must change with it.** That
/// coupling is inherent: the crate exposes no rule identity. The `debug_assert`
/// at the bottom is the safety net — it asserts the branch chosen here is
/// consistent with the returned `Ownership`, so a divergence fails a debug
/// build rather than silently lying in a log.
pub fn rule_name(
    ctx: &DownContext,
    policy: &Policy,
    ownership: Ownership,
    sequence_compositor_owned: bool,
    arming_eligible: usize,
) -> &'static str {
    let rule = if policy.disabled {
        "1_disabled"
    } else if ctx.is_pen && !ctx.mod_down {
        "2_pen_draws"
    } else if ctx.eraser {
        "3_eraser_is_a_tool"
    } else if ctx.mod_down {
        "4_mod_is_the_escape_hatch"
    } else if ctx.in_exclusion_rect {
        "5_exclusion_rect"
    } else if ctx.under_gesture_app && ctx.zone.is_none() {
        "6_passthrough_app"
    } else if ctx.zone.is_some() {
        "7_zone"
    } else if policy.min_gesture_fingers >= 2
        && ctx.counts_toward_arming()
        && arming_eligible >= (policy.min_gesture_fingers - 1) as usize
    {
        "8_arming"
    } else if sequence_compositor_owned {
        "9_sequence_ownership"
    } else {
        "9_sequence_ownership_client"
    };

    // Cross-check the derived step against the verdict the crate actually
    // returned. Steps 1,2,3,5,6 and 9-client all yield Client; 4,7,8 and
    // 9-compositor yield Compositor. A mismatch means the chain above has
    // drifted from the crate's.
    let expect = match rule {
        "1_disabled"
        | "2_pen_draws"
        | "3_eraser_is_a_tool"
        | "5_exclusion_rect"
        | "6_passthrough_app"
        | "9_sequence_ownership_client" => Ownership::Client,
        "4_mod_is_the_escape_hatch" | "7_zone" | "8_arming" | "9_sequence_ownership" => {
            Ownership::Compositor
        }
        _ => Ownership::Ignored,
    };
    debug_assert_eq!(
        expect, ownership,
        "rule_name disagreed with the arbitrator's verdict; \
         touch_arbitrate::decide's precedence has probably changed"
    );

    rule
}

/// smithay logical point to the recogniser's own point.
fn core_point(p: Point<f64, Logical>) -> CorePoint {
    CorePoint::new(p.x, p.y)
}

/// Translate the config's tuning vocabulary into the recogniser's.
pub fn recognizer_config(config: &Config) -> RecognizerConfig {
    let t = &config.input.touchscreen;
    RecognizerConfig {
        swipe_threshold: t.swipe_trigger_distance(),
        pinch_threshold: t.pinch_trigger_distance(),
        // The config knob is written in degrees because that is what a human
        // writes; `rotation_trigger_angle` already converts to radians.
        rotate_threshold: t.rotation_trigger_angle(),
        edge_threshold: t.edge_start_distance(),
        tap_wobble_threshold: t.tap_wobble_threshold(),
        tap_max_duration: Duration::from_millis(t.tap_timeout_ms().round() as u64),
        ..RecognizerConfig::default()
    }
}

/// The ownership policy implied by the current config.
///
/// A malformed `exclusion-rects` is logged and ignored rather than panicking:
/// this is read from the compositor's hot path and also on every config
/// reload, and a config error must not take the compositor down. The parse
/// error itself is reported by niri's normal config validation at startup.
pub fn policy(config: &Config) -> Policy {
    let exclusion_rects = match config.input.touchscreen.exclusion_rects() {
        Ok(rects) => rects
            .into_iter()
            .map(|(w, h, x, y)| Rect { x, y, w, h })
            .collect(),
        Err(err) => {
            warn!(error = %err, "invalid touchscreen exclusion-rects; ignoring");
            Vec::new()
        }
    };

    Policy {
        disabled: config.input.touchscreen.off,
        // How many contacts arm the compositor when none of them started in
        // a zone or under Mod. Configured as
        // `min-gesture-fingers N` under `input { touchscreen { gestures { } } }`,
        // range 1..=10, default 3. The default reads MIN_FINGERS from
        // niri_config::binds, so it cannot drift below the floor the bind
        // parser enforces — a swipe that could be bound is a swipe that can
        // arm, and two fingers stay reserved for client scroll/zoom
        // passthrough.
        min_gesture_fingers: config.input.touchscreen.min_gesture_fingers(),
        // Gesture exclusion rects, in the SAME global logical space as
        // `DownContext::pos`. That is the space `compute_touch_location`
        // produces (output-local point + the output's location in global
        // space), so a rect authored against the layout a user sees will
        // match. Mixing spaces here would make the feature silently no-op on
        // any multi-monitor setup.
        exclusion_rects,
        ..Policy::default()
    }
}

thread_local! {
    /// The compositor's long-lived ownership ledger.
    ///
    /// This is deliberately NOT a fresh `Arbitrator` per touch-down. The
    /// arbitrator's sequence ledger is the whole mechanism by which the
    /// third finger of a swipe can arm the compositor after the first two
    /// were already forwarded to the client; construct a new one per down
    /// and every decision sees an empty sequence, so a multi-finger swipe
    /// starting on a plain surface can never arm and late fingers silently
    /// reset to `Client`. That is the same class of bug as niri-tablet's
    /// patches 0010/0011.
    ///
    /// It lives in a thread-local rather than on `State` because
    /// `src/niri.rs` is owned by another agent working in this tree. niri's
    /// input path is single-threaded (one calloop event loop), so this is
    /// equivalent to a field on the state struct.
    ///
    /// # Invariant: no slots after a sequence ends
    ///
    /// **Once any event ends a touch sequence, this ledger holds no slots.**
    /// It outlives individual sequences, so a slot that is recorded but never
    /// retired stays forever. A stale `Client` slot inflates the arming tally,
    /// so the next unrelated touch — anywhere, on any surface — can arm the
    /// compositor out of nowhere, and `clear_clients` will hand back a slot
    /// the client never had down, which the adapter then tries to close.
    ///
    /// Every path that can end a sequence must therefore reach this ledger:
    /// ordinary `up`, `cancel`, device removal, and the global cancels that
    /// `do_action` and output reconfiguration issue. See `release_slot` and
    /// `reset_persistent`; `reset_persistent` is the blunt instrument for
    /// paths where per-slot ownership is unknown, and it is the correct answer
    /// there because a stale ledger is worse than a lost one.
    static TOUCH_ARBITRATOR: std::cell::RefCell<Arbitrator> =
        std::cell::RefCell::new(Arbitrator::new(Policy::default()));
}

thread_local! {
    /// Every contact physically on the glass, with the geometry the
    /// recogniser needs to be able to reconstruct the sequence.
    ///
    /// Once the compositor owns a sequence it must feed the recogniser
    /// *every* contact that is down, not just the ones whose down the
    /// recogniser happened to witness. Arming happens mid-sequence: the third
    /// finger takes the gesture, and the first two are still on the glass —
    /// they were closed at the *client*, which stops them being phantom
    /// touches but does not lift the fingers. Feeding only the arming contact
    /// hands the recogniser one finger for a three-finger gesture, and with
    /// `min_fingers` at 2 nothing is ever recognised. Edge starts hid this
    /// because they arm on finger 1, when there is no earlier sequence to
    /// reconstruct.
    ///
    /// Recorded on every non-grabbed down that produced a `DownContext`,
    /// client-owned contacts included — they have to be, since a mid-screen
    /// swipe only arms once the client already holds the earlier fingers —
    /// and retired on every lift.
    ///
    /// # Invariant: one entry per contact on the glass, gone when it lifts
    ///
    /// **For every contact on the glass there is exactly one entry, and when
    /// it lifts that entry is gone.** Both halves, because they fail
    /// differently and one without the other is worse than not recording at
    /// all:
    ///
    /// * Recording too little makes `feed_sequence` feed the recogniser a
    ///   partial sequence. A mid-screen swipe is then fed its arming finger
    ///   alone, `min_fingers` is not met, and the headline gesture silently
    ///   never fires.
    /// * Recording too much makes `live_contacts` a fiction: the ledger grows
    ///   without bound and every later `feed_sequence` feeds the recogniser
    ///   contacts that lifted long ago, which is just as silent and just as
    ///   wrong.
    ///
    /// `forget_live_contact` therefore has to be reached by every path that
    /// lifts, and `reset_persistent` by every path that ends a sequence
    /// without naming the slots — see the invariant on `TOUCH_ARBITRATOR`,
    /// which this ledger shares its retirement paths with.
    ///
    /// This is a record of the glass, not of the gesture: a stylus resting on
    /// the screen is an entry here even though it is not a finger. Keeping
    /// tool contacts and filtering them at the reader —
    /// [`TouchArbitrationGrab::feed_sequence`] skips anything that is not a
    /// gesture input — keeps "what is touching" and "what the gesture is
    /// made of" as separate questions, and keeps the recognition rule next
    /// to the code that builds the recogniser's view.
    static TOUCH_CONTACTS: std::cell::RefCell<Vec<LiveContact>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// One contact that is physically down on the glass.
#[derive(Debug, Clone, Copy)]
pub struct LiveContact {
    /// Backend slot id.
    pub slot: u32,
    /// Where it landed, in global logical pixels.
    pub pos: CorePoint,
    /// The edge/corner zone it landed in, if any.
    pub zone: Option<touch_core::Zone>,
    /// When it landed.
    pub time: Duration,
    /// The contact came from a stylus rather than a finger.
    ///
    /// Carried rather than filtered at record time because
    /// [`LiveContact::counts_as_gesture_input`] needs it together with
    /// `mod_down`, and Mod is a property of the sequence at the moment of
    /// the down, not of the tool alone.
    pub is_pen: bool,
    /// The contact is an eraser tip — a distinct tool, not a finger.
    pub is_eraser: bool,
    /// A Mod binding was held when this contact landed.
    pub mod_down: bool,
}

impl LiveContact {
    /// Whether this contact is one of the fingers the gesture is made of.
    ///
    /// Mirrors the tool rules in `touch-arbitrate::decide` rather than
    /// inventing a second opinion, because the arbitrator and the
    /// recogniser have to agree about what a finger is. A resting stylus
    /// that the arbitrator refused to count — steps 2 and 3 of `decide` —
    /// must not be handed to the recogniser either, or the gesture reports
    /// a finger count it was never made of and its centroid and spread are
    /// taken over a contact that is not a finger.
    ///
    /// The `mod_down` term is what keeps the two views in step: a pen under
    /// Mod is a deliberate gesture input, decided at step 4 of `decide` as
    /// [`Ownership::Compositor`], so it is fed. An eraser is not, and step 3
    /// of `decide` places it *before* the Mod escape hatch deliberately, so
    /// no Mod here either.
    pub fn counts_as_gesture_input(&self) -> bool {
        if self.is_eraser {
            return false;
        }
        !self.is_pen || self.mod_down
    }
}

/// Record a contact as physically down.
pub fn note_live_contact(ctx: &DownContext, slot: u32, time: Duration) {
    TOUCH_CONTACTS.with(|contacts| {
        let mut contacts = contacts.borrow_mut();
        contacts.retain(|c| c.slot != slot);
        contacts.push(LiveContact {
            slot,
            pos: ctx.pos,
            zone: ctx.zone,
            time,
            is_pen: ctx.is_pen,
            is_eraser: ctx.eraser,
            mod_down: ctx.mod_down,
        });
    });
}

/// Retire a contact that has lifted.
pub fn forget_live_contact(slot: u32) {
    TOUCH_CONTACTS.with(|contacts| contacts.borrow_mut().retain(|c| c.slot != slot));
}

/// Put a contact back into the ledger after a synthetic seat-level teardown
/// that does not correspond to a lift.
///
/// Closing a contact at the seat to stop it being a phantom touch is not the
/// finger leaving the glass, so the ledger entry has to survive it. The
/// entries are restored as they were, not re-derived from the down that
/// created them, because the geometry and timestamp are the reason the entry
/// exists at all.
pub fn restore_live_contacts(contacts: &[LiveContact]) {
    TOUCH_CONTACTS.with(|all| {
        let mut all = all.borrow_mut();
        for contact in contacts {
            if all.iter().any(|c| c.slot == contact.slot) {
                continue;
            }
            all.push(*contact);
        }
    });
}

/// Every contact currently on the glass, in the order they landed.
pub fn live_contacts() -> Vec<LiveContact> {
    TOUCH_CONTACTS.with(|contacts| contacts.borrow().clone())
}

thread_local! {
    /// Actions a touch gesture matched, waiting for the seat to be free.
    ///
    /// # Why actions cannot be dispatched from inside a grab
    ///
    /// `State::do_action` begins with
    ///
    /// ```text
    /// if let Some(touch) = self.niri.seat.get_touch() { touch.cancel(self); }
    /// ```
    ///
    /// and `touch.cancel` locks the seat's touch `Mutex`. Every
    /// [`TouchGrab`] method — `down`, `up`, `motion`, `frame`, `cancel` — is
    /// invoked by smithay from inside `with_grab`, which already holds that
    /// same lock. It is a plain `std::sync::Mutex`, so it is not reentrant
    /// and the second acquisition blocks forever.
    ///
    /// Calling `do_action` from [`TouchArbitrationGrab::dispatch`] therefore
    /// deadlocks the input thread the first time any touchscreen bind fires.
    /// This was latent for the whole life of the file: no touch bind could
    /// ever fire, because arming was unreachable (see the gate in
    /// `on_touch_down`), so `dispatch` never got as far as an action. Fixing
    /// arming is exactly what made it reachable.
    ///
    /// So `dispatch` queues instead of dispatching, and the touch event
    /// handlers in `mod.rs` drain the queue once they are back outside
    /// `with_grab`. Draining outside the lock is the entire point.
    ///
    /// Order is preserved, so several actions matched by one gesture run in
    /// the order the recogniser produced them.
    static PENDING_ACTIONS: std::cell::RefCell<Vec<Action>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Queue an action a touch gesture matched, to be run once the seat is free.
///
/// See [`PENDING_ACTIONS`] for why this cannot simply call
/// [`crate::input::State::do_action`] inline.
pub fn queue_pending_action(action: Action) {
    PENDING_ACTIONS.with(|pending| pending.borrow_mut().push(action));
}

/// Take every queued action, for the caller to run outside the grab lock.
pub fn take_pending_actions() -> Vec<Action> {
    PENDING_ACTIONS.with(|pending| std::mem::take(&mut *pending.borrow_mut()))
}

/// Run every queued action through `State::do_action`.
///
/// Must only be called from outside `with_grab`: the whole reason these
/// actions are queued is that the touch lock is held while the grab runs.
pub fn flush_pending_actions(state: &mut crate::input::State) {
    for action in take_pending_actions() {
        state.do_action(action, true);
    }
}

thread_local! {
    /// The most recent gesture the recogniser actually produced.
    ///
    /// # What this is for
    ///
    /// Everything else the outside world can see about a touchscreen gesture
    /// is downstream of the recogniser: a bind fired, or an action ran. None
    /// of that says what the recogniser *saw*. Asking "was that really a
    /// three-finger swipe?" by configuring a `fingers=3` bind and a
    /// `fingers=4` bind and asserting which one fired is an argument, not a
    /// measurement, and it goes wrong silently if the bind table changes.
    ///
    /// [`last_recognised_gesture`] is the authoritative answer to "what did
    /// the recogniser actually see", as distinct from "did an action fire".
    ///
    /// # Why it is cheap
    ///
    /// `Option<RecognisedGesture>` over two `Copy` fields, so recording is a
    /// single `Cell::set`: no allocation, no lock, no refcount. It happens a
    /// handful of times per gesture.
    ///
    /// # Why it is cleared by `begin_sequence`
    ///
    /// This is a thread-local in a single-threaded input path, so it outlives
    /// the sequence that set it — and, in the test harness, outlives the test
    /// case. A value that survives lets one gesture answer another's question,
    /// and the resulting failure reads like a compositor bug rather than a
    /// harness bug. That exact confusion has already happened once here.
    ///
    /// It is cleared by [`begin_sequence`], on ENTRY to a sequence, and
    /// deliberately NOT by [`reset_persistent`]. That looks like the wrong
    /// owner — `reset_persistent` is the one place that clears the other two
    /// ledgers — but it runs at END of sequence, after the recogniser has
    /// dispatched the gesture's `End`, so clearing there erases the value the
    /// seam exists to report. Clearing on entry is what makes "the last
    /// gesture" observable at all: a test that gestures then reads gets that
    /// gesture, and a test that never gestures gets `None` rather than a
    /// previous test's value.
    static LAST_RECOGNISED_GESTURE: std::cell::Cell<Option<RecognisedGesture>> =
        const { std::cell::Cell::new(None) };
}

/// What the recogniser most recently recognised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecognisedGesture {
    /// How many contacts the gesture was actually made of.
    ///
    /// This is the field that matters: it is the recogniser's own count, so
    /// a value of 3 means the recogniser genuinely held three contacts, not
    /// that some bind happened to be configured for three.
    pub fingers: u8,
    /// The gesture's shape, as a stable lowercase name.
    pub kind: &'static str,
}

/// The most recent gesture the recogniser produced, or `None` if none has
/// been recognised since the last [`reset_persistent`].
///
/// A deliberate test and diagnostic seam, not an IPC surface. See
/// [`LAST_RECOGNISED_GESTURE`].
pub fn last_recognised_gesture() -> Option<RecognisedGesture> {
    LAST_RECOGNISED_GESTURE.with(|last| last.get())
}

/// Reset all persistent touch bookkeeping.
///
/// For paths that end a sequence without telling us which slots it held —
/// device removal, the global cancel in `do_action`, output reconfiguration.
/// Clears the ledger outright rather than trying to identify slots, because
/// a stale ledger arms phantom gestures while a merely-unnecessary clear only
/// costs the ledger a sequence's worth of history that is about to be
/// invalidated anyway.
///
/// # What this deliberately does NOT clear
///
/// The recognised-gesture seam, which is cleared by [`begin_sequence`]
/// on entry to a sequence instead. See the comment in the body for why:
/// this function runs after the recogniser has already dispatched the
/// gesture, so clearing there would destroy the value the seam reports.
pub fn reset_persistent() {
    TOUCH_ARBITRATOR.with(|arb| {
        arb.borrow_mut().clear();
    });
    TOUCH_CONTACTS.with(|contacts| {
        contacts.borrow_mut().clear();
    });
    // The recognised-gesture seam is deliberately NOT cleared here, even
    // though this function is "clear all persistent touch bookkeeping".
    //
    // This runs at the END of a sequence — the grab's `unset` and `cancel`
    // both reach it — which is *after* the recogniser has dispatched the
    // gesture's `End`. Clearing here erases the very value the seam exists to
    // report, and every caller sees `None`.
    //
    // That is not hypothetical: it is what happened, twice, each time the
    // clear was placed here. `three_finger_swipe_arms_and_fires_the_bind`
    // failed deterministically with "the three-finger swipe must have been
    // recognised" on a gesture that had been recognised perfectly well.
    //
    // The seam is cleared by [`begin_sequence`] instead, on ENTRY to a
    // sequence. That ordering makes the value attributable to the gesture it
    // came from, and it is early enough to also cover a caller that resets
    // state and has not yet started a sequence: a fresh fixture has
    // recognised nothing, so it reads `None` regardless.
}

/// [`reset_persistent`], with the IPC trace event published around it.
///
/// The counts are sampled *before* the clear, because after it there is
/// nothing left to count. Every caller that clears the ledger goes through
/// here rather than calling `reset_persistent` directly, so the trace cannot
/// miss a clear — a clear the observer never sees is exactly the stale-ledger
/// case the trace exists to expose.
pub fn reset_persistent_traced(state: &mut State) {
    let open_client_touches = open_client_touches();
    let live_contacts = live_contacts().len();
    reset_persistent();
    state.ipc_touch_sequence_reset(open_client_touches as u32, live_contacts as u32);
}

/// End a touch sequence because one of its devices went away.
///
/// The teardown-then-clear pair, in that order, and the order is the whole
/// point. A device yanked mid-sequence never delivers the `up` for the
/// contacts it held, so the sequence can only be closed wholesale — but
/// closing it by clearing the ledgers *alone* is what this function exists to
/// prevent.
///
/// # Why the clear cannot stand on its own
///
/// `TOUCH_CONTACTS` and `TOUCH_ARBITRATOR` are not device-scoped: neither
/// `LiveContact` nor the arbitrator's slot key carries a device id, and the
/// key-space note on [`TouchDeviceInfo::touch_device_id`] explains why adding
/// one is not available — the seat itself merges two devices' contacts before
/// any of this runs. So there is no way to clear "the removed device's
/// entries" out of these ledgers; the only two coherent states are *everybody
/// is still on the glass* and *nobody is*.
///
/// Clearing them under a live grab makes the ledgers claim the glass is empty
/// while the grab's `fed_slots`, `compositor_slots` and `client_slots` still
/// claim contacts are down and routed. The grab is still installed whenever
/// any other touch device remains, so the next motion on a *surviving*
/// digitiser runs [`TouchArbitrationGrab::check_invariants`] against that
/// disagreement and panics — in every debug and test build, on a real user
/// pulling a cable. Which assertion trips first depends on how much of the
/// grab survived: measured, it is "fed slots routed or retro-fed and still
/// down" (`:1264`), because `up` empties the routing sets while nothing ever
/// retires the fed set. "routed slots are on the glass" (`:1301`) is the
/// other half of the same disagreement and trips on any path that leaves a
/// slot routed but off the glass.
///
/// So the sequence ends, not just the ledger. `cancel` closes the open
/// gesture, empties the routing sets and the ledgers *together*, and unsets
/// the grab; clearing again afterwards is then a no-op that costs nothing and
/// keeps the trace identical to the other sequence-ending paths. This is the
/// same pair `on_touch_cancel` and `do_action` already run.
///
/// The seat keeps its touch capability when other devices remain — that is
/// correct and unchanged. What does not survive is the sequence: once one of
/// its devices is gone, no ledger state derived from it can be kept, so the
/// next sequence starts from a clean seat.
pub fn end_sequence_on_device_loss(state: &mut State) {
    if let Some(touch) = state.niri.seat.get_touch() {
        touch.cancel(state);
    }

    // `cancel` above already emptied both ledgers, and it did so while the
    // grab was still alive and consistent. This second call is what makes the
    // trace honest for a loss that arrived with no grab installed at all —
    // the case where `cancel` had nothing to run and the ledgers would
    // otherwise keep the departed device's arming tally forever.
    reset_persistent_traced(state);
}

/// Retire one slot from the persistent ledger.
///
/// Called on ordinary touch-up. Returns whether the slot was actually
/// tracked, so an out-of-order lift can be logged rather than silently
/// swallowed.
pub fn release_slot(slot: u32) -> bool {
    TOUCH_ARBITRATOR.with(|arb| arb.borrow_mut().up(slot).is_some())
}

/// [`release_slot`], with the IPC trace event published around it.
pub fn release_slot_traced(state: &mut State, slot: u32) {
    let was_tracked = release_slot(slot);
    state.ipc_touch_slot_retired(slot, was_tracked);
}

/// Publish one ownership decision on the IPC event stream, if the trace is
/// enabled.
///
/// The rule name comes from [`rule_name`] — the same function, and the same
/// arguments, as the `rule=` field of the `debug!` logs at the two decision
/// sites. Deriving it twice would let the log and the event drift apart,
/// which is the one thing a debugging surface must not do.
pub fn publish_arbitration_decision(
    state: &mut State,
    ctx: &DownContext,
    policy: &Policy,
    ownership: Ownership,
    slot: u32,
    seq: &touch_arbitrate::SequenceState,
) {
    let rule = rule_name(
        ctx,
        policy,
        ownership,
        seq.compositor_owned,
        seq.arming_eligible,
    );

    state.ipc_touch_arbitration_decision(
        slot,
        match ownership {
            Ownership::Compositor => TouchOwnership::Compositor,
            Ownership::Client => TouchOwnership::Client,
            Ownership::Ignored => TouchOwnership::Ignored,
        },
        rule.to_string(),
        (seq.arming_eligible + 1) as u32,
        seq.arming_eligible as u32,
        open_client_touches() as u32,
        seq.compositor_owned,
        ctx.is_pen,
        // The decision fires on a finger-down, so the down position is the
        // current position at emission time. No tracking needed.
        ctx.pos.x,
        ctx.pos.y,
    );
}

/// How many client-visible touches the persistent ledger currently holds.
pub fn open_client_touches() -> usize {
    TOUCH_ARBITRATOR.with(|arb| arb.borrow().open_client_touches())
}

/// Decide one down against the persistent ledger.
///
/// Returns the ruling and the slots the caller must cancel at the client
/// before installing a gesture grab: when this finger arms the compositor,
/// any earlier contact already forwarded to the client is a phantom touch
/// waiting to happen, and the only thing that closes it is a touch-end. The
/// list comes from the arbitrator's own ledger (`clear_clients`) rather than
/// from bookkeeping kept here, so the two can never drift apart.
pub fn decide_persistent(
    config: &Config,
    slot: u32,
    ctx: &DownContext,
) -> (
    Ownership,
    Arbitrator,
    Vec<u32>,
    touch_arbitrate::SequenceState,
) {
    let policy = policy(config);
    TOUCH_ARBITRATOR.with(|arb| {
        let mut arb = arb.borrow_mut();
        if *arb.policy() != policy {
            arb.set_policy(policy);
        }
        // Ask the ledger the question `decide` will ask, rather than
        // reimplementing the tally — the crate exposes this precisely so a
        // caller that wants to log the decision cannot get it subtly wrong.
        let seq = arb.sequence_state(ctx);
        let ownership = arb.down(slot, ctx);

        // Only an arming gesture has anything to cancel. A plain client
        // down, or a pen contact, leaves the ledger alone.
        let to_cancel = if ownership == Ownership::Compositor {
            arb.clear_clients()
        } else {
            Vec::new()
        };

        (ownership, arb.clone(), to_cancel, seq)
    })
}

/// Fill in everything the arbitrator needs about one touch-down at `pos`.
///
/// `output` is the output the contact landed on. Zone classification is
/// relative to that output's edges rather than the global bounding box, so a
/// finger in the middle of a left-hand monitor is not an edge swipe. `None`
/// means the contact is off every output, where there are no edges to zone
/// against and only the kill switch and the window rule can apply.
pub fn down_context(
    state: &State,
    pos: Point<f64, Logical>,
    output: Option<&Output>,
    mod_down: bool,
    device: (u32, bool, bool),
    policy: &Policy,
) -> DownContext {
    let zone_px = state
        .niri
        .config
        .borrow()
        .input
        .touchscreen
        .edge_start_distance();
    let zone = output.and_then(|output| {
        state
            .niri
            .global_space
            .output_geometry(output)
            .and_then(|geo| {
                let size = OutputSize::new(geo.size.w as f64, geo.size.h as f64);
                let within = pos - geo.loc.to_f64();
                touch_core::classify_zone(core_point(within), size, zone_px)
            })
    });

    // Window-rule passthrough. The lookup shape mirrors the rest of the
    // compositor: find the mapped window at the point, then ask its resolved
    // rules.
    let under_gesture_app = state
        .niri
        .window_under(pos)
        .is_some_and(|mapped| mapped.rules().touchscreen_gesture_passthrough);

    DownContext {
        zone,
        under_gesture_app,
        mod_down,
        is_pen: device.1,
        eraser: device.2,
        device_id: device.0,
        pos: core_point(pos),
        in_exclusion_rect: policy.excludes(core_point(pos)),
    }
}

/// Baseline for the progress numbers published on one gesture.
///
/// Held between the gesture's `Begin` and `End` so each `Update` can report
/// how far the gesture moved since the last event and how fast it was doing
/// so. The recognizer itself reports only an absolute progress, and it
/// reports it without a clock; the rates are ours to compute.
struct PublishedProgress {
    /// Progress reported by the previous published event.
    last_progress: f64,
    /// When that event was published.
    last_time: Instant,
}

/// The gesture grab: installed when the compositor claims a touch, torn down
/// when the last compositor-owned contact lifts.
///
/// The grab is per-sequence, not per-finger. It holds exactly one
/// [`Recognizer`] and one [`Arbitrator`] and routes every subsequent event
/// by asking the arbitrator which layer owns that slot.
pub struct TouchArbitrationGrab {
    start_data: AnyStartData<State>,
    recognizer: Recognizer,
    arbitrator: Arbitrator,
    /// Slots currently owned by the client and forwarded verbatim.
    ///
    /// Tracked alongside the arbitrator's ledger rather than instead of it,
    /// because the ledger is keyed by `u32` while the forward path is driven
    /// by `TouchSlot`, and because the cancel audit below needs the two to be
    /// cross-checkable.
    client_slots: HashSet<TouchSlot>,
    /// Slots the compositor owns. The grab is uninstalled when the last one
    /// lifts.
    compositor_slots: HashSet<TouchSlot>,
    /// Slots already fed to the recogniser, so `feed_sequence` is idempotent.
    fed_slots: HashSet<touch_core::Slot>,
    /// Slots whose seat-level touch-end is being issued right now.
    ///
    /// Closing a stranded contact at the seat re-enters this grab's own
    /// `up`, which retires the slot from both ledgers — including the live
    /// ledger, which `route_down` restores afterwards. So for the duration
    /// of that call the slot is genuinely routed-but-not-on-the-glass, and
    /// the invariant checker would otherwise fire on the compositor's own
    /// correct teardown.
    ///
    /// Naming the window rather than suppressing checks wholesale is the
    /// point: outside this set every assertion is live, and inside it only
    /// the two liveness assertions are relaxed, and only for the specific
    /// slot being closed.
    seat_close_in_flight: HashSet<TouchSlot>,
    /// Bookkeeping for the in-flight gesture's published progress.
    ///
    /// Only populated while a gesture whose bind carries `emit-progress
    /// true` is live. Without a bind asking for progress this stays `None`
    /// and no IPC event is ever built.
    published: Option<PublishedProgress>,
    /// Whether the cancel audit has already emitted its one release line.
    ///
    /// Set-and-consumed by [`TouchGrab::cancel`]. The point is a hard bound
    /// of one log line per grab instance, so a persistent disagreement
    /// cannot turn into per-contact spam on the input hot path. Reset to
    /// `false` when the grab is constructed.
    cancel_audit_reported: bool,
    /// Whether [`TouchGrab::unset`] has already run on this instance.
    ///
    /// smithay calls `unset` TWICE for one teardown, and both calls are in
    /// its own code: `TouchInnerHandle::unset_grab` (rev `19c16d3`,
    /// `src/input/touch/mod.rs:469-471`) invokes `handler.unset(data)` and
    /// then calls `self.inner.unset_grab`, which invokes `handler.unset(data)`
    /// AGAIN (`mod.rs:570-575`) because `self.grab` is still
    /// `GrabStatus::Active` there. Grab takeover double-calls the same way:
    /// `TouchInnerHandle::set_grab` (`mod.rs:461-462`) unsets the OLD handler
    /// and `TouchInternal::set_grab` (`mod.rs:564-566`) unsets it once more.
    ///
    /// The body below is not idempotent in the one place that matters:
    /// `reset_persistent_traced` publishes a `sequence_reset` IPC event on
    /// every call, and that event documents itself as "a wholesale clear of
    /// the touch ledger" (`src/ipc/server.rs:1232-1239`). Two of them for one
    /// clear makes the trace claim a reset happened twice, and the second
    /// carries the post-reset counts (`0 open, 0 live`) — a line that reads as
    /// "the ledger was reset" when nothing was left to reset. The clearing
    /// itself is idempotent and no consumer pairs a reset with a start, so
    /// nothing downstream is confused; the defect is that this file's own
    /// debugging surface lies about the teardown it is instrumenting, which
    /// is the one thing a trace must not do (see the note on
    /// [`publish_arbitration_decision`]).
    ///
    /// A per-instance flag is the guard, and it is safe in the direction that
    /// matters: a takeover unsets the OLD grab, whose flag is then set, and
    /// the NEW grab is constructed fresh by [`Self::new`] with the flag
    /// `false`, so its own teardown still publishes. Nothing legitimate is
    /// suppressed, and the single-call path is byte-for-byte unchanged
    /// because the flag is only ever set after the body has run.
    ///
    /// Same shape as [`Self::cancel_audit_reported`]: one hard bound per grab
    /// instance, on a method smithay may call more than once.
    unset_done: bool,
}

impl TouchArbitrationGrab {
    /// Build the grab for a sequence whose first slot the caller has already
    /// arbitrated.
    ///
    /// The ruling and its slot come from the caller because the decision is
    /// made in `on_touch_down`, before a grab can exist; re-deciding here
    /// would mean two arbiters disagreeing about the same finger.
    pub fn new(
        start_data: AnyStartData<State>,
        arbitrator: Arbitrator,
        first_slot: TouchSlot,
        first_ownership: Ownership,
        recognizer_config: RecognizerConfig,
    ) -> Self {
        let mut client_slots = HashSet::new();
        let mut compositor_slots = HashSet::new();
        match first_ownership {
            Ownership::Compositor => {
                compositor_slots.insert(first_slot);
            }
            Ownership::Client => {
                client_slots.insert(first_slot);
            }
            Ownership::Ignored => {}
        }

        Self {
            start_data,
            recognizer: Recognizer::new(recognizer_config),
            arbitrator,
            client_slots,
            compositor_slots,
            fed_slots: HashSet::new(),
            seat_close_in_flight: HashSet::new(),
            cancel_audit_reported: false,
            unset_done: false,
            published: None,
        }
    }

    /// Feed every contact physically on the glass into the recogniser.
    ///
    /// This is the whole-sequence rule, and it is the thing that makes a
    /// mid-screen three-finger swipe work at all. The sequence starts when
    /// the first finger lands, but the compositor only takes it when a later
    /// finger arms; by then there are several contacts down whose `down`
    /// events were routed to the client and never reached the recogniser.
    ///
    /// Closing those contacts at the seat does not lift them — the fingers are
    /// still touching the glass — so the recogniser has to be told about them
    /// retroactively, at their real positions and with their real timestamps,
    /// or it is looking at a one-finger gesture and will never cross
    /// `min_fingers`. Feeding them in landing order matters: the recogniser
    /// derives the tap window and the baseline centroid from the order of
    /// arrivals.
    ///
    /// Contacts that are not fingers are skipped rather than fed, so the
    /// recogniser's finger count and centroid are computed over the same
    /// contacts the arbitrator counted when it armed. See
    /// [`LiveContact::counts_as_gesture_input`]. A skipped slot is left out
    /// of `fed_slots` as well, so it is not silently half-fed if Mod is
    /// pressed and released mid-gesture.
    ///
    /// Idempotent per slot, so calling it again after a later finger joins
    /// re-seeds without duplicating.
    pub fn feed_sequence(&mut self) {
        for contact in live_contacts() {
            if !contact.counts_as_gesture_input() {
                continue;
            }
            let slot = touch_core::Slot::new(contact.slot);
            if self.fed_slots.contains(&slot) {
                continue;
            }
            self.fed_slots.insert(slot);
            self.recognizer
                .down(slot, contact.pos, contact.zone, contact.time);
        }

        // Retro-feeding is the one place `fed_slots` and the routing sets
        // are *designed* to disagree, so the check has to run here too: this
        // is where a contact starts belonging to neither set while still
        // being on the glass.
        self.check_invariants("feed_sequence");
    }

    /// Sorted slot ids, for a deterministic diagnostic.
    ///
    /// A `HashSet`'s `Debug` prints in iteration order, so two runs of the
    /// same bug would print different sets and a bug report comparing them
    /// would be comparing noise. Sorted ids make the message stable.
    #[cfg(debug_assertions)]
    fn sorted_slots(slots: &HashSet<TouchSlot>) -> Vec<u32> {
        let mut ids: Vec<u32> = slots.iter().map(|s| slot_id(*s)).collect();
        ids.sort_unstable();
        ids
    }

    /// Sorted ids of what the recogniser has actually been fed.
    #[cfg(debug_assertions)]
    fn sorted_fed(&self) -> Vec<u32> {
        let mut ids: Vec<u32> = self.fed_slots.iter().map(|s| s.id()).collect();
        ids.sort_unstable();
        ids
    }

    /// Report one violated invariant.
    ///
    /// # Why this panics only in debug
    ///
    /// This is `debug_assert!` semantics, not a debug/release split between
    /// panic and log, and the difference matters. These checks run on every
    /// contact, and several of the conditions they test — disjointness,
    /// ledger-versus-routing agreement — are not momentary: once broken they
    /// stay broken for the whole gesture. A release `warn!` here would emit
    /// once per touch event for as long as the condition held, which is
    /// journal spam on the input hot path, costs work in the build that
    /// actually runs on the user's hardware, and makes per-contact log
    /// grepping *less* useful. Nobody acts on a warning they have seen four
    /// hundred times in one gesture.
    ///
    /// So in release this is compiled out entirely. `cargo test` builds with
    /// debug assertions on by default and is the consumer that matters: a
    /// test that runs green past a broken ledger is worse than no test at
    /// all, because it certifies the bug as correct behaviour. The hardware
    /// gate is a human, and a human debugging a gesture cannot act on a line
    /// they cannot reproduce and cannot correlate — so the release story is
    /// the doc comment on the failing bind, not a log line.
    ///
    /// The one exception is the cancel audit in [`TouchGrab::cancel`], which
    /// predates this function and stays visible in release because a
    /// disagreement there means the *seat* is wrong, not just our view of
    /// it. That one is rate-limited per grab; see the comment there.
    ///
    /// The message carries the phase, the invariant name, all three routing
    /// sets and the ledger counts, because "invariant violated" costs a
    /// debugging session on its own.
    #[cfg(debug_assertions)]
    fn invariant_violation(&self, phase: &str, invariant: &str, detail: String) {
        panic!(
            "touch arbitration invariant `{invariant}` violated in {phase}: \
             {detail}\n  compositor_slots={:?} client_slots={:?} \
             fed_slots={:?}\n  live_contacts={:?} \
             persistent_open_client_touches={} \
             grab_arbitrator_open_client_touches={}",
            Self::sorted_slots(&self.compositor_slots),
            Self::sorted_slots(&self.client_slots),
            self.sorted_fed(),
            live_contacts().iter().map(|c| c.slot).collect::<Vec<_>>(),
            open_client_touches(),
            self.arbitrator.open_client_touches(),
        );
    }

    /// Release no-op twin of [`Self::invariant_violation`].
    ///
    /// Present so the call sites need no `cfg` of their own. The `detail`
    /// argument is deliberately not even formatted here: in release the
    /// string construction is the dominant cost, and dropping it is the
    /// whole point of compiling these checks out.
    #[cfg(not(debug_assertions))]
    fn invariant_violation(&self, _phase: &str, _invariant: &str, _detail: String) {}

    /// Whether `slot` is the documented carve-out: a contact the recogniser
    /// was retro-fed at arming, whose seat-level touch-end has already been
    /// sent, and whose finger is still on the glass.
    ///
    /// This is the only legal way for a slot to be in `fed_slots` while
    /// belonging to neither routing set. It exists because closing a
    /// contact at the seat stops it being a phantom touch but does not lift
    /// the finger, so the recogniser still has to hear about that finger's
    /// motion and its eventual lift. Naming it here, rather than leaving
    /// the invariant with an unexplained exception, is the difference
    /// between a carve-out and tribal knowledge.
    #[cfg(debug_assertions)]
    fn retro_fed(&self, slot: u32) -> bool {
        let seat_slot = TouchSlot::from(Some(slot));
        self.fed_slots.contains(&touch_core::Slot::new(slot))
            && !self.compositor_slots.contains(&seat_slot)
            && !self.client_slots.contains(&seat_slot)
    }

    /// Check the invariants that hold at *every* point in a sequence.
    ///
    /// `phase` names the call site, so a failure says which transition broke
    /// rather than just that something is inconsistent somewhere.
    ///
    /// The invariants, and why each is safe to assert here rather than only
    /// at a sequence boundary:
    ///
    /// 1. **`compositor_slots` and `client_slots` are disjoint.** A slot in both would be forwarded
    ///    to the client *and* fed to the recogniser — the phantom-touch shape from both directions
    ///    at once.
    /// 2. **Every slot in `fed_slots` is routed or is the [`retro_fed`](Self::retro_fed) carve-out,
    ///    and a carve-out is still on the glass.** A fed slot that is routed nowhere and is no
    ///    longer down is a contact the recogniser can never retire, which freezes the gesture and
    ///    stops the grab from ever uninstalling.
    /// 3. **Every routed slot is still on the glass.** The mirror image: a slot routed to a party
    ///    that has already lifted is a phantom touch that party will never see released.
    /// 4. **Every gesture-input contact on the glass is accounted for** by one of the three sets.
    ///    The live ledger is global and the routing sets are per-sequence; this is what stops a
    ///    mid-screen arming from taking over a sequence while quietly dropping the fingers that
    ///    were already down.
    /// 5. **The grab's arbitrator agrees with `client_slots`** — in count and slot by slot. These
    ///    are two records of one fact, kept apart because the ledger is keyed by `u32` and the
    ///    forward path by `TouchSlot`; a disagreement means a `clear_clients` teardown would close
    ///    the wrong number of touches.
    /// 6. **No fed slot is a tool.** `feed_sequence` skips anything that is not a gesture input, so
    ///    a fed pen or eraser means the recogniser was handed a tool as a finger. No *routing*
    ///    invariant can see this — the slot is legitimately routed — so it needs a direct
    ///    fed-versus-tool comparison.
    ///
    /// What this deliberately does *not* assert is that the recogniser holds
    /// every contact the compositor owns: a contact that lands mid-gesture
    /// and is ruled the client's is routed to the client and never fed, and
    /// that is correct — the compositor never took it.
    #[cfg(debug_assertions)]
    pub fn check_invariants(&self, phase: &str) {
        let compositor = Self::sorted_slots(&self.compositor_slots);
        let client = Self::sorted_slots(&self.client_slots);
        let fed = self.sorted_fed();

        // A slot whose seat-level close is mid-flight is temporarily off the
        // live ledger while still listed in `compositor_slots`: the seat
        // close re-enters this grab's `up`, and `route_down` restores the
        // ledger entry after that call returns. That window is the
        // compositor's own teardown, not drift, so the two liveness
        // assertions below are relaxed for it — and only for it, and only
        // for the specific slot inside the window. Every other assertion
        // stays live throughout.
        let in_flight = |slot: &u32| {
            self.seat_close_in_flight
                .contains(&TouchSlot::from(Some(*slot)))
        };

        // 1. Disjointness.
        if let Some(slot) = compositor.iter().find(|s| client.contains(s)) {
            self.invariant_violation(
                phase,
                "routing sets disjoint",
                format!("slot {slot} is compositor-owned and client-owned at once"),
            );
        }

        // 2. Fed slots are routed, or are the retro-fed carve-out — and a carve-out's finger is
        //    still down.
        for slot in &fed {
            if compositor.contains(slot) || client.contains(slot) {
                continue;
            }
            if (self.retro_fed(*slot) || in_flight(slot))
                && live_contacts().iter().any(|c| c.slot == *slot)
            {
                continue;
            }
            self.invariant_violation(
                phase,
                "fed slots routed or retro-fed and still down",
                format!(
                    "slot {slot} was fed to the recogniser but is in neither \
                     routing set and has no live contact; the recogniser can \
                     never retire it, so the grab can never uninstall"
                ),
            );
        }

        // 6. No fed slot is a tool. `feed_sequence` skips anything that is not a gesture input, so
        //    a fed slot whose live contact is a pen or an eraser can only mean the recogniser was
        //    handed a tool as a finger: it now counts a finger it was never made of, and the
        //    gesture's centroid and spread are taken over a contact that is not one. There is no
        //    *routing* invariant that can see this — the slot is legitimately routed, so only a
        //    direct fed-versus-tool comparison catches it.
        for slot in &fed {
            let Some(contact) = live_contacts().into_iter().find(|c| c.slot == *slot) else {
                // Already reported by assertion 2.
                continue;
            };
            if contact.counts_as_gesture_input() {
                continue;
            }
            self.invariant_violation(
                phase,
                "no tool is fed to the recogniser",
                format!(
                    "slot {slot} is a pen={} eraser={} mod_down={} contact \
                     and was fed to the recogniser as a finger; the \
                     recogniser now counts a finger it was never made of",
                    contact.is_pen, contact.is_eraser, contact.mod_down,
                ),
            );
        }

        // 3. Routed slots are still down.
        for slot in compositor.iter().chain(client.iter()) {
            if live_contacts().iter().any(|c| c.slot == *slot) {
                continue;
            }
            if in_flight(slot) {
                continue;
            }
            self.invariant_violation(
                phase,
                "routed slots are on the glass",
                format!(
                    "slot {slot} is routed but has no live contact; the party \
                     it was routed to is holding a touch that has lifted"
                ),
            );
        }

        // 4. Every gesture-input contact on the glass is accounted for.
        for contact in live_contacts() {
            // A tool is recorded on the glass but never belongs to the
            // gesture. See `LiveContact::counts_as_gesture_input`.
            if !contact.counts_as_gesture_input() {
                continue;
            }
            if compositor.contains(&contact.slot)
                || client.contains(&contact.slot)
                || fed.contains(&contact.slot)
            {
                continue;
            }
            self.invariant_violation(
                phase,
                "glass contacts are routed or fed",
                format!(
                    "slot {} is on the glass and counts as a gesture input, \
                     but is in no routing set and was never fed to the \
                     recogniser; its motion and lift would be dropped",
                    contact.slot
                ),
            );
        }

        // 5. The two records of what the seat was told must agree.
        let open = self.arbitrator.open_client_touches();
        if open != client.len() {
            self.invariant_violation(
                phase,
                "arbitrator client count matches client_slots",
                format!(
                    "the grab's arbitrator holds {open} client-visible touches \
                     but the seat was told {} are open; a `clear_clients` \
                     teardown would close the wrong number",
                    client.len()
                ),
            );
        }
        for slot in &client {
            if self.arbitrator.ownership(*slot) != Some(Ownership::Client) {
                self.invariant_violation(
                    phase,
                    "arbitrator records every forwarded slot as client-owned",
                    format!(
                        "slot {slot} was forwarded to the client but the \
                         grab's arbitrator does not record it as client-owned"
                    ),
                );
            }
        }
    }

    /// Release no-op twin of [`Self::check_invariants`].
    ///
    /// Present so the call sites carry no `cfg` of their own. These checks
    /// run per contact and several of the conditions they test persist for a
    /// whole gesture, so in release they are compiled out rather than logged:
    /// see [`Self::invariant_violation`] for the full reasoning.
    #[cfg(not(debug_assertions))]
    pub fn check_invariants(&self, _phase: &str) {}

    /// Release no-op twin of [`Self::check_sequence_ended`].
    #[cfg(not(debug_assertions))]
    pub fn check_sequence_ended(&self, _phase: &str) {}

    /// Check the invariants that only hold once the sequence is over.
    ///
    /// Kept separate from [`Self::check_invariants`] because they are false
    /// *during* a sequence by construction: a live sequence is supposed to
    /// be holding slots.
    ///
    /// This is where the two thread-local invariants are actually checked —
    /// "no slots after a sequence ends" on `TOUCH_ARBITRATOR`, and "gone
    /// when it lifts" on `TOUCH_CONTACTS`. Both ledgers outlive the grab, so
    /// a slot recorded but never retired survives here, keeps the arming
    /// tally inflated, and arms the next unrelated touch anywhere on any
    /// surface.
    ///
    /// # What this deliberately does NOT assert
    ///
    /// It does not assert that the grab's own `arbitrator` clone is empty.
    /// An earlier version did, and it was a false positive: `unset` clears
    /// the three routing sets and the persistent ledgers but never calls
    /// `arbitrator.clear()`, and it does not need to. By the time `unset`
    /// returns, `handle.unset_grab` has dropped the grab, and the clone goes
    /// with it. Asserting it there would panic on correct code.
    ///
    /// The clone genuinely matters *during* a sequence, which is what
    /// [`Self::check_invariants`] assertion 5 covers, and in `cancel`, where
    /// `arbitrator.clear()` really is called and its return count is audited
    /// against `client_slots`. That is the only place the clone outlives
    /// anything, so that is the only place it is audited.
    ///
    /// # Known blind spot
    ///
    /// Nothing in this file can assert that a contact the seat was told to
    /// close actually *reached the client*. `wl_touch` delivers inside a
    /// `wl_touch.frame`, and a close emitted without a following frame sits
    /// in the compositor's outbound buffer undelivered — a failure that is
    /// invisible from every ledger here. All of these checks can be green
    /// while a client believes a finger is still down. The client-side
    /// recorder in the test harness is the only detector for that class,
    /// which is exactly why it is worth having.
    #[cfg(debug_assertions)]
    pub fn check_sequence_ended(&self, phase: &str) {
        if !self.compositor_slots.is_empty() {
            self.invariant_violation(
                phase,
                "sequence end clears compositor_slots",
                format!(
                    "sequence ended holding compositor slots {:?}",
                    Self::sorted_slots(&self.compositor_slots)
                ),
            );
        }
        if !self.client_slots.is_empty() {
            self.invariant_violation(
                phase,
                "sequence end clears client_slots",
                format!(
                    "sequence ended holding client slots {:?}",
                    Self::sorted_slots(&self.client_slots)
                ),
            );
        }
        if !self.fed_slots.is_empty() {
            self.invariant_violation(
                phase,
                "sequence end clears fed_slots",
                format!("sequence ended with fed slots {:?}", self.sorted_fed()),
            );
        }

        let open_client = open_client_touches();
        if open_client != 0 {
            self.invariant_violation(
                phase,
                "sequence end clears the persistent ownership ledger",
                format!(
                    "{open_client} slots still held by the persistent \
                     arbitrator; they inflate the arming tally of the next \
                     unrelated touch, and `clear_clients` will hand back a \
                     slot the client never had down"
                ),
            );
        }
        let live = live_contacts();
        if !live.is_empty() {
            self.invariant_violation(
                phase,
                "sequence end clears the live-contact ledger",
                format!(
                    "TOUCH_CONTACTS still holds slots {:?}; it grows without \
                     bound and every later `feed_sequence` feeds the \
                     recogniser contacts that lifted long ago",
                    live.iter().map(|c| c.slot).collect::<Vec<_>>()
                ),
            );
        }
    }

    /// Hand recognised events to niri's bind table, and to the IPC event
    /// stream if the matched bind asked for progress.
    ///
    /// The two are independent. Progress publication never fires an action —
    /// `Begin` and `Update` still map to `None` in `gesture_action`, and a
    /// cancelled `End` still maps to `None` — so adding a bind with
    /// `emit-progress true` cannot double-fire it.
    fn dispatch(&mut self, state: &mut State, events: Vec<GestureEvent>) {
        for event in events {
            // Record what the recogniser actually saw, FIRST, before the
            // progress publish and before the bind lookup — so it captures
            // what the recogniser produced and not what survived downstream
            // filtering. See `LAST_RECOGNISED_GESTURE`.
            let gesture = match &event {
                GestureEvent::Begin(g)
                | GestureEvent::Update(g)
                | GestureEvent::End { gesture: g, .. } => g,
            };
            LAST_RECOGNISED_GESTURE.with(|last| {
                last.set(Some(RecognisedGesture {
                    fingers: gesture.fingers(),
                    kind: gesture_kind_name(gesture),
                }));
            });

            self.publish_progress(state, &event);

            let Some(action) = gesture_action(&state.niri.config.borrow(), &event) else {
                continue;
            };
            // Queued, not dispatched. We are inside `with_grab`, which holds
            // the seat's touch lock, and `do_action` starts by taking that
            // same non-reentrant lock. See `PENDING_ACTIONS`.
            queue_pending_action(action);
        }
    }

    /// Mirror one recognised event onto the IPC event stream, if the bind it
    /// resolves to carries `emit-progress true`.
    fn publish_progress(&mut self, state: &mut State, event: &GestureEvent) {
        let gesture = match event {
            GestureEvent::Begin(gesture)
            | GestureEvent::Update(gesture)
            | GestureEvent::End { gesture, .. } => gesture,
        };

        let Some(trigger) = trigger_for(gesture) else {
            return;
        };

        // The borrow of the config ends with this statement; the emit
        // below needs `&mut State`.
        let wants = {
            let config = state.niri.config.borrow();
            bind_for_trigger(&config, &trigger).is_some_and(|bind| bind.emit_progress)
        };
        if !wants {
            return;
        }

        let name = trigger_name(&trigger);

        // A tap has no continuous extent, so the recognizer reports no
        // progress for it. `1.0` is the honest number: a tap is either
        // instantaneous or it did not happen.
        let progress = gesture.progress().unwrap_or(1.0);

        match event {
            GestureEvent::Begin(_) => {
                self.published = Some(PublishedProgress {
                    last_progress: progress,
                    last_time: Instant::now(),
                });
                state.ipc_touch_gesture_begin(
                    name,
                    gesture.fingers(),
                    gesture_displacement(gesture),
                );
            }
            GestureEvent::Update(_) => {
                let now = Instant::now();
                let Some(published) = &mut self.published else {
                    // Unreachable in practice: the recognizer always emits a
                    // `Begin` first. Emitting nothing beats emitting a
                    // velocity computed against a missing baseline.
                    return;
                };
                let delta = progress - published.last_progress;
                let elapsed = now.duration_since(published.last_time).as_secs_f64();
                let velocity = if elapsed > 0.0 { delta / elapsed } else { 0.0 };
                published.last_progress = progress;
                published.last_time = now;
                state.ipc_touch_gesture_update(
                    name,
                    delta,
                    progress,
                    velocity,
                    gesture_displacement(gesture),
                );
            }
            GestureEvent::End { completed, .. } => {
                self.published = None;
                state.ipc_touch_gesture_end(
                    name,
                    progress,
                    *completed,
                    gesture_displacement(gesture),
                );
            }
        }
    }

    /// Arbitrate and route one late-joining contact.
    ///
    /// The device identity of the first finger is not recoverable here — the
    /// grab is installed per-sequence and smithay's `down` carries no device
    /// — so a stylus joining a finger-led sequence mid-gesture is treated as
    /// a finger. In practice a pen contact is arbitrated client-owned the
    /// moment it lands and never joins a compositor sequence.
    fn route_down(
        &mut self,
        state: &mut State,
        handle: &mut TouchInnerHandle<'_, State>,
        focus: Option<(<State as SeatHandler>::TouchFocus, Point<f64, Logical>)>,
        event: &DownEvent,
    ) {
        // The sequence's first contact was arbitrated in `on_touch_down`,
        // before this grab existed, and fed to the recogniser there. The
        // trailing `handle.down` at that call site re-enters this method with
        // the same slot; arbitrating it again would double-count the contact
        // and overwrite the ruling that installed this grab.
        if self.compositor_slots.contains(&event.slot) || self.client_slots.contains(&event.slot) {
            return;
        }

        let policy = self.arbitrator.policy().clone();
        let ctx = late_down_context(state, event.location, &policy);
        let time = Duration::from_micros(event.time.micros());

        // Record before arbitrating: the contact is on the glass either way,
        // so if this very down is what arms the sequence, `feed_sequence`
        // below can hand the recogniser the whole thing including this one.
        note_live_contact(&ctx, slot_id(event.slot), time);

        let seq = self.arbitrator.sequence_state(&ctx);
        let ownership = self.arbitrator.down(slot_id(event.slot), &ctx);
        debug!(
            slot = slot_id(event.slot),
            ownership = ?ownership,
            rule = rule_name(
                &ctx,
                &policy,
                ownership,
                seq.compositor_owned,
                seq.arming_eligible,
            ),
            fed = self.fed_slots.len() + 1,
            "touch ownership decided (late-joining contact)"
        );

        publish_arbitration_decision(state, &ctx, &policy, ownership, slot_id(event.slot), &seq);

        match ownership {
            Ownership::Compositor => {
                self.compositor_slots.insert(event.slot);
                let stranded = self.arbitrator.clear_clients();
                if !stranded.is_empty() {
                    // Arming took the sequence back from contacts already
                    // forwarded to the client. Close them at the seat so they
                    // are not phantom touches, then feed the recogniser every
                    // contact still down — including the ones just closed,
                    // whose fingers did not actually leave the glass.
                    for slot in &stranded {
                        let slot = TouchSlot::from(Some(*slot));

                        // Close it at the client so it is not a phantom, AND
                        // take it over for routing. These are two different
                        // facts and both are true: the client no longer holds
                        // this touch, but the finger is still on the glass, so
                        // its motion and its eventual lift must reach the
                        // recogniser. Leaving it in neither routing set froze
                        // it: its motion was dropped by `motion`, its lift was
                        // dropped by `up`, the contact never left the
                        // recogniser's ContactSet, and `compositor_slots` could
                        // never empty — so the grab never uninstalled and the
                        // stuck contact poisoned the next gesture.
                        self.client_slots.remove(&slot);
                        self.compositor_slots.insert(slot);

                        // `handle.up` re-enters this grab's own `up`, which
                        // retires the slot from both ledgers — correct for a
                        // real lift, wrong here. The finger did not move, so
                        // the entries have to be put back afterwards or
                        // `feed_sequence` two lines down feeds the recogniser
                        // a gesture with the stranded contacts missing.
                        let saved = live_contacts()
                            .into_iter()
                            .find(|c| c.slot == slot_id(slot))
                            .into_iter()
                            .collect::<Vec<_>>();

                        // Bracket the seat close so the invariant checker
                        // knows this slot is mid-teardown: the re-entrant
                        // `up` below observes the slot as routed but off the
                        // glass, which is true for exactly this window and
                        // false everywhere else.
                        self.seat_close_in_flight.insert(slot);
                        handle.up(
                            state,
                            &smithay::input::touch::UpEvent {
                                slot,
                                serial: SERIAL_COUNTER.next_serial(),
                                time: event.time,
                            },
                        );
                        self.seat_close_in_flight.remove(&slot);

                        restore_live_contacts(&saved);
                    }
                    debug!(
                        slot = slot_id(event.slot),
                        cancelled = ?stranded,
                        "touch gesture armed; cancelling stranded client contacts"
                    );

                    // Same reasoning as the call-site path in
                    // `on_touch_down`: these touch-ends are synthesised by
                    // the compositor, not delivered by libinput, and
                    // `wl_touch` only hands events to the client inside a
                    // `wl_touch.frame`. Without an explicit flush the
                    // client keeps a phantom finger until the next
                    // unrelated input batch happens to frame, which makes
                    // the phantom-touch fix contingent on timing. Match
                    // `on_touch_frame`'s call shape.
                    handle.frame(state);
                }
                self.feed_sequence();
            }
            Ownership::Client => {
                self.client_slots.insert(event.slot);
                handle.down(state, focus, event);
            }
            // Dropped: the contact is inert. Not forwarding is the point.
            Ownership::Ignored => {}
        }

        // Every transition that can move a slot between the sets, take one
        // out of both, or invent the retro-fed carve-out ends here.
        self.check_invariants("route_down");
    }
}

/// The down-context for a finger that lands inside an already-installed grab.
///
/// Mod is re-read live rather than remembered: a gesture that began without
/// Mod and gained it mid-flight should be arbitrated the way a fresh touch
/// would be, and holding a stale flag is how a Mod released during a
/// four-finger swipe keeps swallowing the next touch.
fn late_down_context(state: &State, pos: Point<f64, Logical>, policy: &Policy) -> DownContext {
    let zone_px = state
        .niri
        .config
        .borrow()
        .input
        .touchscreen
        .edge_start_distance();

    let zone = state.niri.output_under(pos).and_then(|(output, _)| {
        state
            .niri
            .global_space
            .output_geometry(output)
            .and_then(|geo| {
                let size = OutputSize::new(geo.size.w as f64, geo.size.h as f64);
                let within = pos - geo.loc.to_f64();
                touch_core::classify_zone(core_point(within), size, zone_px)
            })
    });

    let under_gesture_app = state
        .niri
        .window_under(pos)
        .is_some_and(|mapped| mapped.rules().touchscreen_gesture_passthrough);

    DownContext {
        zone,
        under_gesture_app,
        mod_down: mod_down(state),
        pos: core_point(pos),
        in_exclusion_rect: policy.excludes(core_point(pos)),
        ..DownContext::default()
    }
}

/// Re-derive the modifier flag from the live keyboard state.
fn mod_down(state: &State) -> bool {
    let Some(keyboard) = state.niri.seat.get_keyboard() else {
        return false;
    };
    let Some(want) = state.niri.config.borrow().input.mod_key else {
        return false;
    };
    modifiers_from_state(keyboard.modifier_state()).contains(want.to_modifiers())
}

fn modifiers_from_state(mods: ModifiersState) -> Modifiers {
    let mut modifiers = Modifiers::empty();
    if mods.ctrl {
        modifiers |= Modifiers::CTRL;
    }
    if mods.shift {
        modifiers |= Modifiers::SHIFT;
    }
    if mods.alt {
        modifiers |= Modifiers::ALT;
    }
    if mods.logo {
        modifiers |= Modifiers::SUPER;
    }
    modifiers
}

/// Stable lowercase name for a recognised gesture's shape.
///
/// A `&'static str` rather than the formatted `trigger_name`, because the
/// seam must stay a plain `Copy` assignment with no allocation on the
/// dispatch path.
fn gesture_kind_name(gesture: &touch_core::Gesture) -> &'static str {
    use touch_core::Gesture;
    match *gesture {
        Gesture::Swipe { .. } => "swipe",
        Gesture::Pinch { .. } => "pinch",
        Gesture::Rotate { .. } => "rotate",
        Gesture::Tap { .. } => "tap",
        Gesture::TapHold { .. } => "tap-hold",
        Gesture::EdgeSwipe { .. } => "edge-swipe",
        Gesture::CornerSwipe { .. } => "corner-swipe",
    }
}

/// A recognised gesture's displacement along its locked axis.
///
/// Passed through in whatever unit `touch-core` measured the gesture in, and
/// deliberately not renamed to a unit-bearing one: `swipe` and `pinch` are
/// measured in pixels, `rotate` in radians. Converting here would invent a
/// unit the recognizer never used.
///
/// The same exhaustive `match` shape as [`gesture_kind_name`], so a gesture
/// kind added to `touch-core` without a decision here is a compile error
/// rather than a silently-zeroed field.
///
/// `tap` and `tap-hold` are the two kinds with no displacement: neither has
/// a continuous extent, so `0.0` is the honest reading, matching the `1.0`
/// progress [`publish_progress`] already reports for them.
fn gesture_displacement(gesture: &touch_core::Gesture) -> f64 {
    use touch_core::Gesture;
    match *gesture {
        Gesture::Swipe { delta, .. }
        | Gesture::Pinch { delta, .. }
        | Gesture::Rotate { delta, .. }
        | Gesture::EdgeSwipe { delta, .. }
        | Gesture::CornerSwipe { delta, .. } => delta,
        Gesture::Tap { .. } | Gesture::TapHold { .. } => 0.0,
    }
}

/// Resolve a trigger produced by [`trigger_for`] to the bind it names.
///
/// Two of the touchscreen families carry a field the recogniser may leave
/// unset but a user may legitimately omit, so exact equality is not the
/// whole lookup rule for them. Both are "the narrower trigger wins, the
/// unspecified one is the catch-all for that shape":
///
/// - `TouchEdge`: the `zone` is always populated by [`trigger_for`] for an `EdgeSwipe`, but a bind
///   may omit it. That rule is [`Binds::find_touch_edge`]'s and is applied there.
/// - `TouchTapHoldDrag`: the `direction` is populated whenever the recogniser could determine one,
///   but a bind may omit it, which its own doc calls "omnidirectional". So an undirected bind is
///   the catch-all for a directed gesture on the same finger count, exactly as an unzoned bind is
///   for a zoned edge gesture.
///
/// The `TapHold` direction is `None` only when the recogniser could not
/// determine one — a down and a large motion delivered in the same
/// `wl_touch.frame`, before any baseline exists. That is a real, if rare,
/// path, and it resolves to the omnidirectional bind.
fn bind_for_trigger<'a>(config: &'a Config, trigger: &Trigger) -> Option<&'a niri_config::Bind> {
    if let Trigger::TouchEdge { edge, zone } = *trigger {
        return config.binds.find_touch_edge(edge, zone);
    }

    if let Trigger::TouchTapHoldDrag {
        fingers,
        direction: Some(direction),
    } = *trigger
    {
        // Exact first, so a user who restricted their bind to one
        // direction still gets that direction specifically.
        return config
            .binds
            .find(&Trigger::TouchTapHoldDrag {
                fingers,
                direction: Some(direction),
            })
            .or_else(|| {
                config.binds.find(&Trigger::TouchTapHoldDrag {
                    fingers,
                    direction: None,
                })
            });
    }

    config.binds.find(trigger)
}

/// Map a recognised gesture event onto a configured bind, if there is one.
///
/// Only a *completed* `End` dispatches. `Begin` and `Update` are the
/// continuous part of a gesture, consumed by the layout rather than by a
/// bind; firing an action for them too would run every bind twice per
/// gesture.
///
/// An `End` with `completed: false` also dispatches nothing. That covers a
/// gesture the recogniser cancelled and one released before it crossed its
/// trigger threshold — neither actually happened, and firing the bind anyway
/// reads to the user as a bind firing by itself. This matches how niri treats
/// libinput touchpad gestures: a swipe that never latches fires nothing.
///
/// # Why `TouchEdge` does not use the exact-equality lookup
///
/// Every other gesture family is fully determined by its shape, so the
/// trigger this builds is exactly the trigger the user would have written.
/// `TouchEdge` is not: [`trigger_for`] always attaches a `Some(zone)`
/// derived from where along the edge the contact landed, because that is
/// what the recogniser reports. An `EdgeSwipe` therefore emits
/// `TouchEdge { edge, zone: Some(_) }` in every case.
///
/// A user writing the natural, unzoned bind —
///
/// ```kdl
/// TouchEdge edge="bottom" { toggle-overview; }
/// ```
///
/// — has written `zone: None`, so exact equality never matches and the
/// bind never fires. The edge zone is always reported by the recogniser
/// and never omitted by the user, so an unzoned bind is not "no zone",
/// it is "any zone on this edge": which is precisely the fallback rule
/// `Binds::find_touch_edge` was written to express and documents. Using
/// exact equality here bypassed that rule and silently disabled every
/// edge swipe. Corners were unaffected only by accident: they have no
/// third, so they already emit `zone: None`.
fn gesture_action(config: &Config, event: &GestureEvent) -> Option<Action> {
    let trigger = match event {
        GestureEvent::Begin(_) | GestureEvent::Update(_) => return None,
        GestureEvent::End {
            gesture: _,
            completed: false,
        } => return None,
        GestureEvent::End {
            gesture,
            completed: true,
        } => trigger_for(gesture)?,
    };

    bind_for_trigger(config, &trigger).map(|bind| bind.action.clone())
}

fn trigger_for(gesture: &touch_core::Gesture) -> Option<Trigger> {
    use touch_core::{Corner, Edge, Gesture, SwipeDirection as CoreSwipe, Third};

    let swipe_dir = |d| match d {
        CoreSwipe::Up => SwipeDirection::Up,
        CoreSwipe::Down => SwipeDirection::Down,
        CoreSwipe::Left => SwipeDirection::Left,
        CoreSwipe::Right => SwipeDirection::Right,
    };

    Some(match *gesture {
        Gesture::Swipe {
            fingers, direction, ..
        } => Trigger::TouchSwipe {
            fingers,
            direction: swipe_dir(direction),
        },
        Gesture::Pinch {
            fingers, direction, ..
        } => Trigger::TouchPinch {
            fingers,
            direction: match direction {
                touch_core::PinchDirection::In => PinchDirection::In,
                touch_core::PinchDirection::Out => PinchDirection::Out,
            },
        },
        Gesture::Rotate {
            fingers, direction, ..
        } => Trigger::TouchRotate {
            fingers,
            direction: match direction {
                touch_core::RotateDirection::Clockwise => RotateDirection::Cw,
                touch_core::RotateDirection::CounterClockwise => RotateDirection::Ccw,
            },
        },
        Gesture::Tap { fingers } => Trigger::TouchTap { fingers },
        Gesture::TapHold { fingers, direction } => Trigger::TouchTapHoldDrag {
            fingers,
            direction: direction.map(swipe_dir),
        },
        Gesture::EdgeSwipe { edge, third, .. } => Trigger::TouchEdge {
            edge: match edge {
                Edge::Left => ScreenEdge::Left,
                Edge::Right => ScreenEdge::Right,
                Edge::Top => ScreenEdge::Top,
                Edge::Bottom => ScreenEdge::Bottom,
            },
            zone: Some(match third {
                Third::Start => EdgeZone::Start,
                Third::Center => EdgeZone::Center,
                Third::End => EdgeZone::End,
            }),
        },
        // A corner swipe is reported as an edge swipe along the corner's
        // vertical edge, unzoned: `TouchEdge` has no corner variant, and
        // pinning it to one third would be a worse guess than matching the
        // whole edge.
        Gesture::CornerSwipe { corner, .. } => Trigger::TouchEdge {
            edge: match corner {
                Corner::TopLeft | Corner::BottomLeft => ScreenEdge::Left,
                Corner::TopRight | Corner::BottomRight => ScreenEdge::Right,
            },
            zone: None,
        },
    })
}

/// Render a gesture trigger the way it is written in the config, so a client
/// can match the string against what the user actually wrote.
///
/// `niri-ipc` cannot carry `niri_config::Trigger` itself — the two crates do
/// not depend on each other, and niri-ipc is the lower-level one — so the
/// trigger is flattened to this string on the way out. It is a lossy but
/// stable rendering: same trigger, same string, every time.
fn trigger_name(trigger: &Trigger) -> String {
    // The `Debug` names of the config's direction and edge enums are the
    // capitalized spellings of exactly the words the config accepts, so
    // lowercasing them reproduces the documented KDL syntax.
    fn lc(value: impl std::fmt::Debug) -> String {
        format!("{value:?}").to_lowercase()
    }

    match *trigger {
        Trigger::TouchSwipe { fingers, direction } => {
            format!(
                "TouchSwipe fingers={fingers} direction=\"{}\"",
                lc(direction)
            )
        }
        Trigger::TouchPinch { fingers, direction } => {
            format!(
                "TouchPinch fingers={fingers} direction=\"{}\"",
                lc(direction)
            )
        }
        Trigger::TouchRotate { fingers, direction } => {
            format!(
                "TouchRotate fingers={fingers} direction=\"{}\"",
                lc(direction)
            )
        }
        Trigger::TouchTap { fingers } => format!("TouchTap fingers={fingers}"),
        Trigger::TouchTapHoldDrag { fingers, direction } => match direction {
            Some(direction) => format!(
                "TouchTapHoldDrag fingers={fingers} direction=\"{}\"",
                lc(direction)
            ),
            None => format!("TouchTapHoldDrag fingers={fingers}"),
        },
        Trigger::TouchEdge { edge, zone } => match zone {
            Some(zone) => format!("TouchEdge edge=\"{}\" zone=\"{}\"", lc(edge), lc(zone)),
            None => format!("TouchEdge edge=\"{}\"", lc(edge)),
        },
        // `trigger_for` only ever produces the six touchscreen triggers
        // above, so this is unreachable in practice.
        _ => format!("{trigger:?}"),
    }
}

impl TouchGrab<State> for TouchArbitrationGrab {
    fn down(
        &mut self,
        state: &mut State,
        handle: &mut TouchInnerHandle<'_, State>,
        focus: Option<(<State as SeatHandler>::TouchFocus, Point<f64, Logical>)>,
        event: &DownEvent,
    ) {
        self.route_down(state, handle, focus, event);
        self.check_invariants("down");
    }

    fn up(&mut self, state: &mut State, handle: &mut TouchInnerHandle<'_, State>, event: &UpEvent) {
        // Both ledgers: this grab's clone, and the persistent one the call
        // site consults for the next sequence. The persistent ledger is not
        // automatically kept in step — it outlives this grab.
        self.arbitrator.up(slot_id(event.slot));
        release_slot_traced(state, slot_id(event.slot));
        forget_live_contact(slot_id(event.slot));

        let slot = touch_core::Slot::new(slot_id(event.slot));

        // The recogniser is driven by `fed_slots` — "what the recogniser has
        // actually been told about" — rather than by the routing sets. Those
        // two can disagree: a contact can be retro-fed into the recogniser
        // (when arming takes over a finger already sent to the client) while
        // the seat has already been told that finger is up. Keying off
        // `fed_slots` makes it impossible to leave a contact stranded in the
        // recogniser's ContactSet, which is what stops the grab from ever
        // uninstalling and poisons the next gesture.
        let known_to_recognizer = self.fed_slots.remove(&slot);

        // Forward to the client only if it still holds this touch. A
        // retro-fed slot was already closed at the seat when it was stranded,
        // so sending a second touch-end would be a phantom in the other
        // direction.
        let client_holds_it = self.client_slots.remove(&event.slot);
        if client_holds_it {
            handle.up(state, event);
        }

        let compositor_owned = self.compositor_slots.remove(&event.slot);

        // Checked before the early return below, because an unrecognised,
        // unowned lift is exactly the case where the sets must already be
        // back in step with the glass.
        self.check_invariants("up");

        if !known_to_recognizer && !compositor_owned {
            return;
        }

        if known_to_recognizer {
            let time = Duration::from_micros(event.time.micros());
            self.recognizer.up(slot);
            let events = self.recognizer.frame(time);
            self.dispatch(state, events);
        }

        // The sequence ends when the last contact the recogniser is tracking
        // has lifted. Keyed on `fed_slots` for the same reason as above.
        if self.fed_slots.is_empty() {
            // About to tear the grab down. `fed_slots` emptiness is the
            // condition this branch is already keyed on, so the meaningful
            // end-state assertion is the one *around* it: a slot still
            // listed as client-owned must not also still be fed, because
            // that is a contact the seat was told had lifted but the
            // recogniser is still tracking.
            //
            // `client_slots` itself is deliberately *not* asserted empty
            // here. A contact ruled the client's mid-gesture (a passthrough
            // window, an exclusion rect) is routed to the client and never
            // fed, so it can legitimately still be listed when the last
            // *fed* contact lifts and the grab unsets; its own lift then
            // reaches the client through the normal ungrabbed path. Asserting
            // emptiness here would panic on correct behaviour.
            if let Some(slot) = self.client_slots.iter().find(|s| {
                self.fed_slots
                    .contains(&touch_core::Slot::new(slot_id(**s)))
            }) {
                self.invariant_violation(
                    "up:sequence-end",
                    "sequence end leaves no client slot still fed",
                    format!(
                        "slot {} is both client-owned and fed to the \
                         recogniser at sequence end; the seat was told that \
                         finger lifted, and the recogniser still tracks it",
                        slot_id(*slot)
                    ),
                );
            }
            self.check_invariants("up:sequence-end");
            handle.unset_grab(self, state);
        }
    }

    fn motion(
        &mut self,
        state: &mut State,
        handle: &mut TouchInnerHandle<'_, State>,
        focus: Option<(<State as SeatHandler>::TouchFocus, Point<f64, Logical>)>,
        event: &MotionEvent,
    ) {
        // Forward to the client only if it actually holds this touch. A
        // retro-fed slot was closed at the seat when it was stranded, so
        // forwarding would be talking to a client that no longer has it.
        //
        // `motion` does not mutate the routing sets, so the check is not
        // strictly needed here — but this is where a frozen-finger bug
        // becomes visible to the user, and a check that runs only where
        // something changes is a check that is missing at exactly the point
        // a reader goes looking for it.
        if self.client_slots.contains(&event.slot) {
            handle.motion(state, focus, event);
        } else {
            // The recogniser is driven by `fed_slots`, matching `up`. A
            // contact retro-fed at arming is in neither routing set in the
            // naive sense, but it is in `fed_slots` and its motion must
            // reach the recogniser or the gesture tracks frozen fingers.
            let slot = touch_core::Slot::new(slot_id(event.slot));
            if self.fed_slots.contains(&slot) {
                // `Recognizer::motion` returns `false` when the slot was not
                // live, i.e. the tracker and the backend have desynced.
                // `touch-core` says callers must surface that rather than
                // absorb it silently, so it gets named here instead of
                // being dropped — this is how a stuck touch announces
                // itself before it becomes one.
                if !self.recognizer.motion(slot, core_point(event.location)) {
                    warn!(
                        "touch desync: motion for slot {} was not live in the \
                         tracker; the slot is fed to the recognizer but the \
                         contact set does not hold it",
                        slot_id(event.slot)
                    );
                }
            }
        }

        self.check_invariants("motion");
    }

    fn frame(&mut self, state: &mut State, handle: &mut TouchInnerHandle<'_, State>) {
        // Client-owned contacts still need their frame flushed; the
        // compositor-owned ones just need recognition advanced.
        handle.frame(state);

        // smithay's frame carries no timestamp and `touch-core` takes time as
        // an argument rather than reading a clock, so the compositor's
        // monotonic clock supplies it. That keeps tap timing honest without
        // giving the pure crate a time source.
        let events = self.recognizer.frame(crate::utils::get_monotonic_time());
        self.dispatch(state, events);
    }

    fn cancel(&mut self, state: &mut State, handle: &mut TouchInnerHandle<'_, State>) {
        // `handle.cancel` closes every touch the seat has open, which is
        // exactly the set `clear()` counts. Do the count *after* forwarding:
        // a client-visible touch must never be left open across a grab
        // transition, and the returned count is the audit of that. Getting
        // it wrong strands a client's finger down forever, which is the bug
        // this whole layer exists to prevent.
        handle.cancel(state);

        // `reset` closes any in-flight gesture with `completed: false`, so a
        // cancelled sequence never fires its bound action.
        let events = self.recognizer.reset();
        self.dispatch(state, events);

        // The auditor. `handle.cancel` above already closed every touch the
        // seat had open, so the number `clear()` reports *is* the number of
        // client-visible touches that just got closed, and `client_slots` is
        // what we believed the seat was holding. A mismatch means the seat
        // was told the wrong number of touches had ended — a client finger
        // left down forever, which is the bug this whole layer exists to
        // prevent.
        //
        // Rate limiting: the release `warn!` fires **at most once per grab
        // instance**, gated on a flag this grab carries. `cancel` is
        // per-sequence, but a grab can be cancelled and torn down and
        // reinstalled in a tight loop on a flapping touchscreen, so
        // "per cancel" is not by itself a bound on the log volume. Once per
        // grab instance is: the count can be emitted on every occurrence in
        // a debug/test build, where the panic already carries the detail,
        // and the flag is consumed on first emit so a persistent condition
        // costs one line per grab rather than one per contact.
        let closed = self.arbitrator.clear();
        let tracked = self.client_slots.len();
        if closed != tracked {
            if !std::mem::take(&mut self.cancel_audit_reported) {
                warn!(
                    closed,
                    tracked,
                    compositor_slots = self.compositor_slots.len(),
                    fed_slots = self.fed_slots.len(),
                    "touch arbitration: ledger and routing sets disagree on \
                     client touches; reported once per grab"
                );
            }
            // Fatal wherever assertions are live. This was a `warn!` alone
            // and it caught a real bug class in production, which means it
            // fires on states the test suites must never reach: a check that
            // only warns during `cargo test` is a check the tests are
            // allowed to fail silently.
            self.invariant_violation(
                "cancel",
                "cancel closes exactly the client touches the seat was told",
                format!(
                    "the arbitrator closed {closed} client-visible touches but \
                     the seat was told {tracked} are open; a client's finger \
                     is left down, or a touch it never had was closed"
                ),
            );
        }

        // `clear()` above emptied the clone. This is the one place the clone
        // outlives the grab: `cancel` keeps the grab alive until the
        // `unset_grab` at the end of this method, so anything still recorded
        // here would still be reachable by a later event. `unset` gets no
        // such assertion because the grab is dropped the moment it returns;
        // see the note on `check_sequence_ended`.
        if self.arbitrator.open_client_touches() != 0 {
            self.invariant_violation(
                "cancel",
                "cancel clears the grab's arbitrator",
                format!(
                    "{} client-visible touches still open in the grab's \
                     arbitrator after clear()",
                    self.arbitrator.open_client_touches()
                ),
            );
        }

        self.client_slots.clear();
        self.compositor_slots.clear();
        // `recognizer.reset()` above emptied the recogniser's ContactSet, so
        // the fed set must be emptied in step or the next `feed_sequence`
        // would skip contacts it never actually re-fed.
        self.fed_slots.clear();

        // The grab's cancel ends the whole sequence without naming the slots,
        // so the persistent ledger goes with it. See the invariant on
        // `TOUCH_ARBITRATOR`.
        reset_persistent_traced(state);

        // Both ledgers are empty at this point and both routing sets are
        // cleared, so this is the first and only point at which the two
        // thread-local invariants in this file can actually be tested.
        self.check_sequence_ended("cancel");

        handle.unset_grab(self, state);
    }

    fn shape(
        &mut self,
        state: &mut State,
        handle: &mut TouchInnerHandle<'_, State>,
        event: &ShapeEvent,
    ) {
        handle.shape(state, event);
    }

    fn orientation(
        &mut self,
        state: &mut State,
        handle: &mut TouchInnerHandle<'_, State>,
        event: &OrientationEvent,
    ) {
        handle.orientation(state, event);
    }

    fn start_data(&self) -> &TouchGrabStartData<State> {
        self.start_data.unwrap_touch()
    }

    fn unset(&mut self, state: &mut State) {
        // smithay unsets twice per teardown; see `unset_done` for the two
        // call sites that do it. The clearing below is idempotent, but
        // `reset_persistent_traced` publishes one IPC `sequence_reset` per
        // call, so an unguarded second pass reports a second wholesale clear
        // of a ledger that is already empty — a teardown that did not happen,
        // on the surface whose job is to report which ones did.
        if self.unset_done {
            return;
        }
        self.unset_done = true;

        // The sequence ended without a cancel: every finger lifted through
        // `up`, or a newer grab took over. Close any gesture still open so a
        // bind cannot fire from a torn-down grab.
        let events = self.recognizer.reset();
        if !events.is_empty() {
            self.dispatch(state, events);
        }

        self.fed_slots.clear();
        self.client_slots.clear();
        self.compositor_slots.clear();
        // A slot left mid-teardown here would keep relaxing the liveness
        // checks on a grab that no longer exists, so drop the window rather
        // than carry it past teardown.
        self.seat_close_in_flight.clear();

        // A newer grab taking over is a sequence boundary too, and this is the
        // only notification we get for it. Without this the slots this grab
        // owned stay in the persistent ledger and arm the next unrelated
        // touch. Slots that lifted normally were already retired in `up`, so
        // whatever remains here belongs to a sequence that ended abruptly.
        reset_persistent_traced(state);

        self.check_sequence_ended("unset");
    }
}

#[cfg(test)]
mod bind_lookup_tests {
    use super::*;

    fn config(kdl: &str) -> Config {
        Config::parse_mem(kdl).unwrap()
    }

    fn swipe(direction: touch_core::SwipeDirection) -> GestureEvent {
        GestureEvent::End {
            gesture: touch_core::Gesture::Swipe {
                fingers: 3,
                direction,
                delta: 0.0,
                progress: 1.0,
            },
            completed: true,
        }
    }

    /// The natural, undirected `TouchTapHoldDrag` bind must fire on a
    /// directed tap-hold-drag. Pre-fix, `trigger_for` emitted
    /// `direction: Some(_)` and exact equality could not equal the
    /// `direction: None` the user had written, so the bind never fired.
    #[test]
    fn omnidirectional_tap_hold_drag_fires_on_a_directed_gesture() {
        let config = config(
            r#"
            binds {
                TouchTapHoldDrag fingers=3 { toggle-overview; }
            }
            "#,
        );

        let event = GestureEvent::End {
            gesture: touch_core::Gesture::TapHold {
                fingers: 3,
                direction: Some(touch_core::SwipeDirection::Left),
            },
            completed: true,
        };

        assert_eq!(
            gesture_action(&config, &event),
            Some(Action::ToggleOverview),
            "the unzoned/undirected bind is the documented catch-all"
        );
    }

    /// A directed bind must still win over the omnidirectional one, and a
    /// direction the user did not bind must fall back rather than matching
    /// the wrong direction.
    #[test]
    fn a_directed_tap_hold_drag_bind_still_wins() {
        let config = config(
            r#"
            binds {
                TouchTapHoldDrag fingers=3 { toggle-overview; }
                TouchTapHoldDrag fingers=3 direction="left" { close-window; }
            }
            "#,
        );

        let left = GestureEvent::End {
            gesture: touch_core::Gesture::TapHold {
                fingers: 3,
                direction: Some(touch_core::SwipeDirection::Left),
            },
            completed: true,
        };
        assert_eq!(gesture_action(&config, &left), Some(Action::CloseWindow));

        let up = GestureEvent::End {
            gesture: touch_core::Gesture::TapHold {
                fingers: 3,
                direction: Some(touch_core::SwipeDirection::Up),
            },
            completed: true,
        };
        assert_eq!(gesture_action(&config, &up), Some(Action::ToggleOverview));

        // A different finger count is a different gesture entirely.
        let two = GestureEvent::End {
            gesture: touch_core::Gesture::TapHold {
                fingers: 2,
                direction: Some(touch_core::SwipeDirection::Left),
            },
            completed: true,
        };
        assert_eq!(gesture_action(&config, &two), None);
    }

    /// The same catch-all applies when the recogniser could not determine a
    /// direction at all — `direction: None` — which is what a down and a
    /// large motion in one `wl_touch.frame` produce.
    #[test]
    fn undetermined_tap_hold_drag_direction_fires_the_omnidirectional_bind() {
        let config = config(
            r#"
            binds {
                TouchTapHoldDrag fingers=3 { toggle-overview; }
            }
            "#,
        );

        let event = GestureEvent::End {
            gesture: touch_core::Gesture::TapHold {
                fingers: 3,
                direction: None,
            },
            completed: true,
        };
        assert_eq!(
            gesture_action(&config, &event),
            Some(Action::ToggleOverview)
        );
    }

    /// The edge case the original fix covered, re-asserted through the
    /// shared resolver so a regression in either site is caught here.
    #[test]
    fn an_unzoned_edge_bind_still_fires() {
        let config = config(
            r#"
            binds {
                TouchEdge edge="bottom" { toggle-overview; }
            }
            "#,
        );

        let event = GestureEvent::End {
            gesture: touch_core::Gesture::EdgeSwipe {
                edge: touch_core::Edge::Bottom,
                third: touch_core::Third::Center,
                delta: 0.0,
                progress: 1.0,
            },
            completed: true,
        };
        assert_eq!(
            gesture_action(&config, &event),
            Some(Action::ToggleOverview)
        );
    }

    /// A direction-restricted bind must not swallow a gesture of a
    /// different family: `publish_progress` routes through the same
    /// resolver, so this asserts the resolve is family-correct.
    #[test]
    fn the_resolver_does_not_cross_match_families() {
        let config = config(
            r#"
            binds {
                TouchSwipe fingers=3 direction="left" { toggle-overview; }
            }
            "#,
        );

        let trigger = trigger_for(&touch_core::Gesture::TapHold {
            fingers: 3,
            direction: Some(touch_core::SwipeDirection::Left),
        })
        .unwrap();
        assert!(
            bind_for_trigger(&config, &trigger).is_none(),
            "a tap-hold must not resolve to a swipe bind"
        );
        assert_eq!(
            bind_for_trigger(
                &config,
                &trigger_for(&touch_core::Gesture::Swipe {
                    fingers: 3,
                    direction: touch_core::SwipeDirection::Left,
                    delta: 0.0,
                    progress: 1.0,
                })
                .unwrap()
            ),
            config.binds.0.first(),
        );
    }

    #[test]
    fn a_plain_swipe_is_unaffected() {
        let config = config(
            r#"
            binds {
                TouchSwipe fingers=3 direction="up" { toggle-overview; }
            }
            "#,
        );
        assert_eq!(
            gesture_action(&config, &swipe(touch_core::SwipeDirection::Up)),
            Some(Action::ToggleOverview)
        );
        assert_eq!(
            gesture_action(&config, &swipe(touch_core::SwipeDirection::Down)),
            None
        );
    }
}

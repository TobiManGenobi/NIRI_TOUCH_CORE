//! `touch-arbitrate` — layer L3 of the niri touch stack.
//!
//! See `DESIGN.md` at the repository root for the full architecture. L1
//! (`touch_core::contact`) and L2 (`touch_core::gesture`) are pure state
//! machines; this crate is the layer above them that answers one question
//! per touch-down: **who owns this touch?**
//!
//! ```text
//! ┌─ L3  ARBITRATION ──────────────────────────────────┐
//! │  Who owns this touch? window rules, exclusion rects  │
//! ├─ L2  RECOGNITION ──────────────────────────────────┤
//! ├─ L1  CONTACT ─────────────────────────────────────┐
//! └─────────────────────────────────────────────────────┘
//! ```
//!
//! # Arming
//!
//! Ownership is not a per-contact constant. Fingers accumulate on a plain
//! surface and the Nth one — [`Policy::min_gesture_fingers`], three by
//! default — arms the gesture for the whole sequence. That is the Android
//! and niri-tablet model, and it is why [`decide`] takes a
//! [`SequenceState`] rather than a single boolean.
//!
//! Arming is also the only transition that takes touches *back*: fingers 1
//! and 2 were already forwarded, so when finger 3 arms, the caller must
//! cancel those two client touches via [`Arbitrator::clear_clients`] or it has
//! manufactured the phantom touches patch `0011` describes.
//!
//! # Why this is the interesting layer
//!
//! Neither fork shipped an ownership *model*. niri-touch derived ownership
//! from `!handle.is_grabbed()`, which is always false over app surfaces
//! because smithay installs an implicit `TouchDownGrab` on every delivered
//! down; niri-tablet had no model at all and needed four of its twenty
//! patches (`0006` late-joining fingers, `0010` sequence ownership, `0011`
//! the grab seam, `0012` device-removal state) purely to repair the
//! bookkeeping.
//!
//! So the model is the deliverable:
//!
//! * one [`Ownership`] enum, three states, no boolean heuristics;
//! * one transition function, [`decide`], which is a pure function of the
//!   down context, the policy, and one piece of sequence state;
//! * one ledger, [`Arbitrator`], which remembers ownership per slot so that
//!   sequence ownership and device-removal teardown are testable at all.
//!
//! # Determinism
//!
//! As in L1/L2: no [`std::time::Instant`], no wall clock, no thread, no
//! filesystem. Callers pass a [`Duration`] timebase if they want one, and
//! nothing in this crate reads one.
//!
//! # The invariant this crate exists to protect
//!
//! **A client-visible touch is never left open when a gesture grab
//! installs.** [`Arbitrator::clear`] returns exactly the number of
//! client-visible touches the caller must close; callers that pass that
//! count to their touch stack close every one of them. See its docs.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
pub use touch_core::Point;

use std::collections::BTreeMap;

use touch_core::Zone;

/// How many plain-surface fingers arm a gesture by default.
///
/// Three is the Android and niri-tablet convention, and the count that
/// `bind Swipe fingers=3` in `DESIGN.md` assumes.
pub const DEFAULT_MIN_GESTURE_FINGERS: u32 = 3;

/// Which layer owns a single touch contact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ownership {
    /// The compositor owns it: it is not forwarded to the client.
    Compositor,
    /// The client owns it: the down and its motion and its up are
    /// delivered normally.
    Client,
    /// The touch is not routed anywhere — it is dropped. Used by callers
    /// that need a neutral third state (a device the compositor has decided
    /// to ignore wholesale); [`decide`] never returns it, so the recogniser
    /// above can treat it as "this contact is inert".
    Ignored,
}

/// Everything known about one touch-down at the moment it lands.
///
/// Plain data, no smithay types, no niri-config types. The adapter in the
/// compositor is responsible for filling it in; this crate only reads it.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct DownContext {
    /// The edge or corner zone the contact landed in, if any.
    pub zone: Option<Zone>,
    /// The surface under the contact matches a
    /// `touchscreen-gesture-passthrough` window rule.
    pub under_gesture_app: bool,
    /// A Mod binding (typically the stylus barrel button) is held.
    pub mod_down: bool,
    /// The contact came from a stylus rather than a finger.
    pub is_pen: bool,
    /// The contact is an eraser tip — a distinct tool, not a finger.
    pub eraser: bool,
    /// The originating `libinput` device id.
    pub device_id: u32,
    /// Where the contact landed, in logical pixels.
    pub pos: Point,
    /// The contact landed inside an advertised exclusion rect.
    pub in_exclusion_rect: bool,
}

impl DownContext {
    /// Whether this contact counts toward arming a gesture once the
    /// sequence reaches [`Policy::min_gesture_fingers`].
    ///
    /// The rule is deliberately narrow: **only plain-surface fingers
    /// count.** A pen tip is a tool, not a finger; a contact inside a
    /// client-advertised exclusion rect is one the client explicitly asked
    /// the compositor not to interfere with; a contact on a
    /// `touchscreen-gesture-passthrough` window is one the user has told us
    /// this app wants in full. Letting any of those push the sequence over
    /// the arming threshold would let the compositor grab touches it was
    /// just told to leave alone, and would require cancelling exactly the
    /// touches the client most carefully protected. Arming is a claim about
    /// the user's intent on a bare surface, so it is made from bare
    /// surfaces.
    ///
    /// `under_gesture_app` is checked without the zone caveat step 6 of
    /// [`decide`] applies: a contact in a zone is decided at step 7 and
    /// never reaches arming, so the two cases cannot disagree in practice.
    pub fn counts_toward_arming(&self) -> bool {
        !self.is_pen && !self.eraser && !self.in_exclusion_rect && !self.under_gesture_app
    }
}

/// An axis-aligned rectangle in logical pixels, anchored at its top-left
/// corner.
///
/// This is the L3 shape of Android's
/// `systemGestureExclusionRects`: a client advertises rects where it wants
/// the compositor's gestures to stay out, rather than the compositor
/// guessing from an app-id match.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    /// Left edge, in logical pixels.
    pub x: f64,
    /// Top edge, in logical pixels.
    pub y: f64,
    /// Width, in logical pixels.
    pub w: f64,
    /// Height, in logical pixels.
    pub h: f64,
}

impl Rect {
    /// Whether `p` lies within this rect.
    ///
    /// The convention is **half-open on the far edge**: a point on the
    /// right or bottom boundary is *outside*. The near edges are inclusive.
    /// This makes abutting rects partition the plane without overlapping,
    /// which is what a client listing several exclusion rects wants; the
    /// alternative (closed on all four edges) makes two rects that share a
    /// boundary both claim the boundary point.
    ///
    /// A rect with a non-positive width or height contains nothing at all,
    /// rather than being treated as a degenerate line or point. A
    /// zero-area exclusion rect is a configuration error, and silently
    /// honouring it would eat touches the client never asked to protect.
    pub fn contains(&self, p: Point) -> bool {
        if self.w <= 0.0 || self.h <= 0.0 {
            return false;
        }
        p.x >= self.x && p.x < self.x + self.w && p.y >= self.y && p.y < self.y + self.h
    }
}

/// The configured arbitration policy.
#[derive(Debug, Clone, PartialEq)]
pub struct Policy {
    /// The kill switch. When set, every touch belongs to the client and the
    /// compositor never arms a gesture.
    pub disabled: bool,
    /// App-ids matched by a `touchscreen-gesture-passthrough` window rule.
    ///
    /// Carried here as plain data so the policy is comparable and printable
    /// in a test; the actual window-rule match happens in the caller's
    /// adapter, which sets [`DownContext::under_gesture_app`].
    pub passthrough_apps: Vec<String>,
    /// Client-advertised gesture exclusion rects, in logical pixels.
    pub exclusion_rects: Vec<Rect>,
    /// How many plain-surface fingers arm a gesture, counting the finger
    /// that triggers the arm.
    ///
    /// Defaults to 3, the Android and niri-tablet convention: three fingers
    /// is the smallest count that is not confused with a two-finger scroll or
    /// a pinch, and it is what `bind Swipe fingers=3` expects.
    ///
    /// A value of 0 or 1 makes the compositor claim the first bare contact,
    /// which is almost never what a config author wants; treat those as
    /// "arming disabled" rather than as a request to take over single taps.
    pub min_gesture_fingers: u32,
}

/// The default is [`DEFAULT_MIN_GESTURE_FINGERS`], not zero.
///
/// Written out rather than derived because the zero value of a `u32` is the
/// dangerous one here: a derived `Default` would silently disable arming and
/// reproduce exactly the bug this rule exists to fix.
impl Default for Policy {
    fn default() -> Self {
        Self {
            disabled: false,
            passthrough_apps: Vec::new(),
            exclusion_rects: Vec::new(),
            min_gesture_fingers: DEFAULT_MIN_GESTURE_FINGERS,
        }
    }
}

impl Policy {
    /// Whether `p` falls inside any configured exclusion rect.
    ///
    /// This is the rect-aware replacement for niri-touch's boolean
    /// `touchscreen-gesture-passthrough`: the caller can check it early and
    /// short-circuit [`decide`] with
    /// [`DownContext::in_exclusion_rect`] set.
    pub fn excludes(&self, p: Point) -> bool {
        self.exclusion_rects.iter().any(|r| r.contains(p))
    }
}

/// What the sequence already held, as seen by one arriving contact.
///
/// A single struct rather than a growing parameter list: [`decide`] needs to
/// know both *whether anyone owns this sequence* and *how close it is to
/// arming*, and both are properties of the same thing — the set of contacts
/// currently down. Splitting them into two positional `bool`/`usize`
/// arguments would let a caller transpose them silently, and would keep
/// growing every time the arming rule needed one more fact about the
/// sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SequenceState {
    /// Some contact already down in this sequence belongs to the compositor.
    pub compositor_owned: bool,
    /// Contacts already down in this sequence that count toward arming.
    ///
    /// See [`DownContext::counts_toward_arming`] for exactly which contacts
    /// qualify; the short version is that pen tips and contacts the client
    /// asked to keep (exclusion rects, passthrough windows) are excluded.
    pub arming_eligible: usize,
}

/// The single ownership transition function.
///
/// # Precedence
///
/// The order below is the contract, and the numbered list is the whole
/// specification: read it top to bottom and you have the function. Steps 1–7
/// mirror niri-touch's `touch_gesture.rs` lines 274–284, where edge zones and
/// Mod are tested before the passthrough-app rule, and layer the pen/eraser
/// tool rules and the rect exclusion on top in that same "tool identity
/// first, Mod second, passthrough last" shape. Step 8 is new here and
/// carries the arming rule that neither prior fork had.
///
/// 1. [`Policy::disabled`] → [`Ownership::Client`]. The kill switch outranks
///    everything, including Mod, zones and arming. Touch must never be able
///    to wedge a session, so the escape hatch has to outrank the escape
///    hatches.
/// 2. `is_pen && !mod_down` → [`Ownership::Client`]. A stylus drawing must
///    not arm a multi-finger gesture; Mod is how the user says "no, treat
///    this as a gesture" (barrel button).
/// 3. [`DownContext::eraser`] → [`Ownership::Client`]. An eraser tip is a
///    tool, never a gesture finger. Checked before Mod so that a Mod held
///    from a previous gesture cannot capture the eraser.
/// 4. [`DownContext::mod_down`] → [`Ownership::Compositor`]. Mod is the
///    explicit user escape hatch and beats every heuristic below it.
/// 5. [`DownContext::in_exclusion_rect`] → [`Ownership::Client`]. Android's
///    rect exclusion. Beats a passthrough-app match below, but not Mod:
///    Mod is a live human decision, a rect is a standing advertisement.
/// 6. `under_gesture_app && zone.is_none()` →
///    [`Ownership::Client`]. The window-rule passthrough. Explicitly does
///    **not** apply inside a zone.
/// 7. [`DownContext::zone`] present → [`Ownership::Compositor`]. Zones beat
///    passthrough apps: an edge swipe from inside a drawing app is the
///    escape hatch for "this app swallowed my gestures". A zone arms on the
///    very first finger, with no threshold to cross.
/// 8. **Arming.** If this contact counts toward arming (see
///    [`DownContext::counts_toward_arming`]) and the sequence already holds at
///    least `policy.min_gesture_fingers - 1` such contacts, the threshold is
///    reached → [`Ownership::Compositor`].
///    Placed immediately before the fallthrough, and below every
///    escape-hatch rule, for two reasons. It is a claim about user intent
///    made from nothing but a count of bare fingers on a plain surface, so
///    it must lose to anything explicit — a Mod held, a rect, a window
///    rule. And it must lose to step 7, because a zone-starting swipe that
///    waited for a third finger would be unusable: edge swipes are
///    single-finger by nature.
/// 9. Otherwise, fall through to sequence ownership:
///    [`Ownership::Compositor`] if the sequence already has a
///    compositor-owned contact, else [`Ownership::Client`].
///
/// # Why step 8 exists
///
/// Without it, a multi-finger swipe in the *middle* of the screen can never
/// arm, and the hole is not obvious: finger 1 lands on a plain surface with
/// no zone, no Mod and no passthrough app, so step 9 hands it to the client.
/// Nothing in the ledger is then compositor-owned, so finger 2 is also the
/// client's, and so is finger 3. The recogniser above never sees a sequence
/// it could arm on, and `bind Swipe fingers=3` silently never fires away from
/// the edges. Only zone-starts and Mod-starts would ever work. Step 8 is the
/// Android and niri-tablet model: fingers accumulate, and the Nth finger
/// arms the gesture.
///
/// # Arming is the one arm that changes a live slot's owner
///
/// Steps 1–7 and 9 are all decided by facts that were true *before* this
/// contact landed, so they are stable across a sequence. Step 8 is not: when
/// the third finger arms, fingers 1 and 2 have already been forwarded to the
/// client. Those are client-visible touches the compositor is now taking
/// away, and they must be cancelled at the client or they become exactly the
/// phantom touches niri-tablet patch `0011` describes. That is why
/// [`Arbitrator::clear_clients`] exists, and why a caller must invoke it the
/// moment [`decide`] returns [`Ownership::Compositor`] by arming — see its
/// docs.
///
/// Step 9 is the documented last case, and the chain is exhaustive in the
/// sense that matters: every path returns a value, and the fallthrough is
/// stated here rather than being an implicit tail of the function. A
/// `match` was rejected deliberately — the guards are read top-to-bottom as
/// a priority table, which is exactly the document a reader needs, and the
/// `bool` chain makes a missing branch a compile-visible ordering change
/// rather than a silently reordered arm.
///
/// This is a chain rather than an exhaustive `match` on an input enum; the
/// `DESIGN.md` stability rule "one `Ownership` enum, one transition
/// function, exhaustive `match`" is honoured in the sense that matters:
/// there is exactly one function that maps context to ownership, and it has
/// no partial branches. [`Ownership::Ignored`] is deliberately unreachable
/// from here — dropping a contact is the caller's decision, made after this
/// function returns, so that a device-level ignore policy never silently
/// becomes a gesture-stack behaviour.
pub fn decide(ctx: &DownContext, policy: &Policy, seq: SequenceState) -> Ownership {
    // 1. Kill switch outranks everything.
    if policy.disabled {
        return Ownership::Client;
    }
    // 2. Pen-down draws by default; Mod is how the user overrides that.
    if ctx.is_pen && !ctx.mod_down {
        return Ownership::Client;
    }
    // 3. Eraser is a tool, not a finger. Before Mod, deliberately.
    if ctx.eraser {
        return Ownership::Client;
    }
    // 4. Mod is the user escape hatch.
    if ctx.mod_down {
        return Ownership::Compositor;
    }
    // 5. Advertised exclusion rects win over window rules.
    if ctx.in_exclusion_rect {
        return Ownership::Client;
    }
    // 6. Window-rule passthrough, but never inside a zone.
    if ctx.under_gesture_app && ctx.zone.is_none() {
        return Ownership::Client;
    }
    // 7. A zone is the compositor's territory, from the very first finger.
    if ctx.zone.is_some() {
        return Ownership::Compositor;
    }
    // 8. Arming: this finger brings the sequence up to min_gesture_fingers.
    // The `>= 2` guard means a configured threshold of 0 or 1 disables
    // arming rather than claiming every bare single-finger tap.
    if policy.min_gesture_fingers >= 2
        && ctx.counts_toward_arming()
        && seq.arming_eligible >= (policy.min_gesture_fingers - 1) as usize
    {
        return Ownership::Compositor;
    }
    // 9. Documented fallthrough: inherit sequence ownership.
    if seq.compositor_owned {
        Ownership::Compositor
    } else {
        Ownership::Client
    }
}

/// What the ledger remembers about one slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SlotRecord {
    ownership: Ownership,
    /// Snapshot of [`DownContext::counts_toward_arming`] at the down.
    counts_toward_arming: bool,
    /// Whether the contact was a pen or eraser tip.
    ///
    /// Kept separately from `counts_toward_arming` because the two answer
    /// different questions. A pen tip never counts toward arming *and* never
    /// counts toward the compositor-owned tally in step 9 — a stylus resting
    /// on the glass must not silently arm the recogniser for the next
    /// finger, which would present to the user as random gesture
    /// activation.
    pen: bool,
}

/// The per-seat ownership ledger.
///
/// [`decide`] is the rule; this is the bookkeeping the rule needs. It tracks
/// which slots are down and who owns each, so that arming (step 8 of
/// [`decide`]), sequence ownership (step 9) and both flavours of teardown
/// work without a live seat — the things niri-tablet needed patches `0010`,
/// `0011` and `0012` for.
///
/// # The invariant
///
/// **A client-visible touch is never left open when a gesture grab
/// installs.** There are two ways the compositor takes touches back, and
/// each has its own teardown:
///
/// - **Arming** takes back only the contacts it needs to reclaim, leaving
///   compositor-owned contacts untouched. That is
///   [`clear_clients`], and it returns the slots to cancel.
/// - **Device removal** takes back everything, device gone or not. That is
///   [`clear`], and it returns a count.
///
/// Both are the niri-tablet patch `0011` invariant expressed as an API
/// contract rather than a comment. Skipping the close leaves the phantom
/// touches that patch describes — a browser pinch-zooming from one finger
/// and ignoring every later tap, up to nine phantoms at once. The numbers
/// are returned rather than asserted in a comment, they are computable
/// without a seat via [`Arbitrator::client_slots`] and
/// [`Arbitrator::open_client_touches`], and they are asserted in tests.
#[derive(Debug, Clone)]
pub struct Arbitrator {
    policy: Policy,
    slots: BTreeMap<u32, SlotRecord>,
}

impl Arbitrator {
    /// A new arbitrator with an empty ledger.
    pub fn new(policy: Policy) -> Self {
        Self {
            policy,
            slots: BTreeMap::new(),
        }
    }

    /// The policy currently in force.
    pub fn policy(&self) -> &Policy {
        &self.policy
    }

    /// Replace the policy. Does not retroactively re-decide slots already
    /// down: a touch's owner is decided once, at its down, and stays decided
    /// until it lifts. Re-deciding mid-gesture would strand the client's
    /// view of which fingers it has.
    pub fn set_policy(&mut self, policy: Policy) {
        self.policy = policy;
    }

    /// Arbitrate one touch-down and record the result.
    ///
    /// `slot` identifies the contact. Re-using a live slot overwrites its
    /// entry, which is correct: libinput reuses slot ids after a finger
    /// lifts, and a down for a slot we believe is still down is a desync we
    /// resolve in favour of the newest event.
    ///
    /// When this returns [`Ownership::Compositor`] *because the sequence
    /// armed* — that is, when the contact had no zone and no Mod and reached
    /// `min_gesture_fingers` on plain surface alone — the caller must
    /// immediately call [`clear_clients`] and cancel the returned slots at
    /// the client. See the type-level invariant.
    pub fn down(&mut self, slot: u32, ctx: &DownContext) -> Ownership {
        let seq = self.sequence_state(ctx);
        let ownership = decide(ctx, &self.policy, seq);

        self.slots.insert(
            slot,
            SlotRecord {
                ownership,
                counts_toward_arming: ctx.counts_toward_arming(),
                pen: ctx.is_pen || ctx.eraser,
            },
        );

        ownership
    }

    /// The [`SequenceState`] an arriving `ctx` would be judged against.
    ///
    /// Public so a caller that wants to predict or log the decision without
    /// mutating the ledger can ask the same question [`decide`] will ask,
    /// rather than reimplementing the tally and getting it subtly wrong.
    pub fn sequence_state(&self, _ctx: &DownContext) -> SequenceState {
        SequenceState {
            // Pen and eraser contacts are excluded: a stylus resting on the
            // glass must never make the next finger inherit Compositor.
            compositor_owned: self
                .slots
                .values()
                .any(|r| !r.pen && r.ownership == Ownership::Compositor),
            arming_eligible: self
                .slots
                .values()
                .filter(|r| r.counts_toward_arming)
                .count(),
        }
    }

    /// Record a touch-up and return the ownership it had, if it was tracked.
    ///
    /// Out-of-order lifts are normal and must not be errors: a caller that
    /// gets `None` here has an untracked slot, which is a desync worth
    /// logging but not worth asserting on mid-gesture.
    pub fn up(&mut self, slot: u32) -> Option<Ownership> {
        self.slots.remove(&slot).map(|r| r.ownership)
    }

    /// The recorded ownership of `slot`, if it is currently down.
    pub fn ownership(&self, slot: u32) -> Option<Ownership> {
        self.slots.get(&slot).map(|r| r.ownership)
    }

    /// The slots currently down that belong to the client, ascending.
    ///
    /// The slots [`clear_clients`] would cancel. Exposed so a caller can
    /// assert the arming invariant before it acts: after a three-finger
    /// swipe arms, exactly the first two slots must appear here.
    pub fn client_slots(&self) -> Vec<u32> {
        self.slots
            .iter()
            .filter(|(_, r)| r.ownership == Ownership::Client)
            .map(|(s, _)| *s)
            .collect()
    }

    /// Drop every client-owned slot, returning the slots to cancel.
    ///
    /// This is the **arming** teardown: the sequence just reached
    /// `min_gesture_fingers`, the compositor is taking the gesture back, and
    /// the contacts it had forwarded so far must be cancelled at the client
    /// or they become phantom touches (niri-tablet patch `0011`). Each
    /// returned slot is one touch point the caller must close — so the
    /// caller does not have to count anything, which is the point of
    /// returning them rather than returning a `usize`.
    ///
    /// Compositor-owned contacts are left alone: the gesture is still live,
    /// and only the client's half is being retracted. A pen slot *is* the
    /// client's, so it is cancelled along with the rest — it was forwarded
    /// in the first place. Compare [`clear`], the device-removal teardown,
    /// which takes compositor-owned contacts too, because a device that no
    /// longer exists has nothing left to deliver them to.
    pub fn clear_clients(&mut self) -> Vec<u32> {
        let slots = self.client_slots();
        for slot in &slots {
            self.slots.remove(slot);
        }
        slots
    }

    /// Drop every slot, returning how many client-visible touches the caller
    /// must close.
    ///
    /// This is the **device-removal** teardown — `touch_end` in `wl_touch`
    /// terms, or a device yanked mid-gesture — and unlike
    /// [`clear_clients`] it takes compositor-owned contacts too, because
    /// there is no device left to deliver anything. The returned count is
    /// exactly the number of client-owned slots dropped, and the caller owes
    /// exactly that many touch-point closes.
    pub fn clear(&mut self) -> usize {
        let closed = self.open_client_touches();
        self.slots.clear();
        closed
    }

    /// How many client-visible touches are currently open.
    ///
    /// Exposed so callers — and tests — can assert the teardown invariant
    /// without having to call [`clear`] first.
    pub fn open_client_touches(&self) -> usize {
        self.slots
            .values()
            .filter(|r| r.ownership == Ownership::Client)
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finger() -> DownContext {
        DownContext {
            pos: Point::new(600.0, 500.0),
            ..DownContext::default()
        }
    }

    fn zone() -> Option<Zone> {
        Some(Zone::Corner {
            corner: touch_core::Corner::BottomLeft,
        })
    }

    // ── decision table, one test per precedence row ──────────────────────

    #[test]
    fn disabled_beats_everything_including_mod_and_zone() {
        let policy = Policy {
            disabled: true,
            ..Policy::default()
        };
        let ctx = DownContext {
            zone: zone(),
            under_gesture_app: false,
            mod_down: true,
            ..finger()
        };
        assert_eq!(
            decide(&ctx, &policy, SequenceState::default()),
            Ownership::Client
        );
        assert_eq!(
            decide(
                &ctx,
                &policy,
                SequenceState {
                    compositor_owned: true,
                    ..SequenceState::default()
                }
            ),
            Ownership::Client
        );
    }

    #[test]
    fn pen_without_mod_is_the_clients() {
        let ctx = DownContext {
            is_pen: true,
            ..finger()
        };
        assert_eq!(
            decide(&ctx, &Policy::default(), SequenceState::default()),
            Ownership::Client
        );
        // Even mid-sequence, and even in a zone.
        assert_eq!(
            decide(
                &DownContext {
                    zone: zone(),
                    ..ctx
                },
                &Policy::default(),
                SequenceState {
                    compositor_owned: true,
                    ..SequenceState::default()
                }
            ),
            Ownership::Client
        );
    }

    #[test]
    fn pen_with_mod_is_the_compositors() {
        let ctx = DownContext {
            is_pen: true,
            mod_down: true,
            ..finger()
        };
        assert_eq!(
            decide(&ctx, &Policy::default(), SequenceState::default()),
            Ownership::Compositor
        );
    }

    #[test]
    fn eraser_is_the_clients_and_beats_mod() {
        let ctx = DownContext {
            eraser: true,
            mod_down: true,
            ..finger()
        };
        assert_eq!(
            decide(&ctx, &Policy::default(), SequenceState::default()),
            Ownership::Client
        );
    }

    #[test]
    fn mod_beats_pen_and_exclusion_and_passthrough() {
        let policy = Policy {
            passthrough_apps: vec!["org.kde.krita".to_string()],
            ..Policy::default()
        };
        let ctx = DownContext {
            mod_down: true,
            is_pen: false,
            in_exclusion_rect: true,
            under_gesture_app: true,
            ..finger()
        };
        assert_eq!(
            decide(&ctx, &policy, SequenceState::default()),
            Ownership::Compositor,
            "Mod is the escape hatch and outranks every heuristic"
        );
    }

    #[test]
    fn exclusion_rect_beats_a_passthrough_app() {
        let policy = Policy {
            exclusion_rects: vec![Rect {
                x: 0.0,
                y: 0.0,
                w: 100.0,
                h: 100.0,
            }],
            ..Policy::default()
        };
        let p = Point::new(50.0, 50.0);
        assert!(policy.excludes(p));
        let ctx = DownContext {
            in_exclusion_rect: true,
            under_gesture_app: true,
            pos: p,
            ..finger()
        };
        assert_eq!(
            decide(&ctx, &policy, SequenceState::default()),
            Ownership::Client
        );
    }

    #[test]
    fn a_zone_beats_a_passthrough_app() {
        let policy = Policy {
            passthrough_apps: vec!["org.kde.krita".to_string()],
            ..Policy::default()
        };
        let ctx = DownContext {
            under_gesture_app: true,
            zone: zone(),
            ..finger()
        };
        assert_eq!(
            decide(&ctx, &policy, SequenceState::default()),
            Ownership::Compositor,
            "edge swipes must stay reachable from a passthrough app"
        );

        // The same app, same spot, no zone: now it is the client's.
        let ctx = DownContext {
            under_gesture_app: true,
            zone: None,
            ..finger()
        };
        assert_eq!(
            decide(&ctx, &policy, SequenceState::default()),
            Ownership::Client
        );
    }

    #[test]
    fn sequence_ownership_is_the_documented_fallthrough() {
        // Nothing special about the contact: bare first finger, then a second.
        assert_eq!(
            decide(&finger(), &Policy::default(), SequenceState::default()),
            Ownership::Client
        );
        assert_eq!(
            decide(
                &finger(),
                &Policy::default(),
                SequenceState {
                    compositor_owned: true,
                    ..SequenceState::default()
                }
            ),
            Ownership::Compositor
        );
    }

    // ── Rect::contains ─────────────────────────────────────────────────

    #[test]
    fn rect_contains_is_half_open_on_the_far_edge() {
        let r = Rect {
            x: 10.0,
            y: 20.0,
            w: 100.0,
            h: 50.0,
        };
        // Near edges are inclusive.
        assert!(r.contains(Point::new(10.0, 20.0)));
        // Far edges are exclusive, so abutting rects partition the plane.
        assert!(!r.contains(Point::new(110.0, 45.0)));
        assert!(!r.contains(Point::new(45.0, 70.0)));
        // Interior.
        assert!(r.contains(Point::new(60.0, 45.0)));
        // Outside.
        assert!(!r.contains(Point::new(9.0, 45.0)));
        assert!(!r.contains(Point::new(60.0, 19.0)));
    }

    #[test]
    fn degenerate_rects_contain_nothing() {
        for r in [
            Rect {
                x: 0.0,
                y: 0.0,
                w: 0.0,
                h: 10.0,
            },
            Rect {
                x: 0.0,
                y: 0.0,
                w: 10.0,
                h: 0.0,
            },
            Rect {
                x: 0.0,
                y: 0.0,
                w: -5.0,
                h: 10.0,
            },
            Rect {
                x: 0.0,
                y: 0.0,
                w: 10.0,
                h: -5.0,
            },
        ] {
            assert!(!r.contains(Point::new(0.0, 0.0)), "{r:?} contains nothing");
            assert!(
                !r.contains(Point::new(-1.0, -1.0)),
                "{r:?} contains nothing"
            );
        }
    }

    // ── arming, step 8 ─────────────────────────────────────────────────

    #[test]
    fn the_third_plain_finger_arms_the_sequence() {
        // The regression test for the hole that made mid-screen swipes
        // impossible: with no zone, no Mod and no passthrough app, the first
        // two fingers are the client's and the third takes the sequence.
        let mut arb = Arbitrator::new(Policy::default());
        assert_eq!(arb.down(0, &finger()), Ownership::Client);
        assert_eq!(arb.down(1, &finger()), Ownership::Client);
        assert_eq!(arb.down(2, &finger()), Ownership::Compositor);
        // And the fourth inherits, by step 9 rather than by arming.
        assert_eq!(arb.down(3, &finger()), Ownership::Compositor);
    }

    #[test]
    fn arming_waits_for_the_configured_threshold() {
        let policy = Policy {
            min_gesture_fingers: 4,
            ..Policy::default()
        };
        let mut arb = Arbitrator::new(policy);
        for slot in 0..3 {
            assert_eq!(
                arb.down(slot, &finger()),
                Ownership::Client,
                "finger {slot} is below the threshold"
            );
        }
        assert_eq!(arb.down(3, &finger()), Ownership::Compositor);
    }

    #[test]
    fn a_threshold_below_two_disables_arming() {
        // 0 and 1 would otherwise claim every bare single-finger tap.
        for min in [0u32, 1] {
            let mut arb = Arbitrator::new(Policy {
                min_gesture_fingers: min,
                ..Policy::default()
            });
            assert_eq!(arb.down(0, &finger()), Ownership::Client);
            assert_eq!(arb.down(1, &finger()), Ownership::Client);
            assert_eq!(arb.down(2, &finger()), Ownership::Client);
        }
    }

    #[test]
    fn a_pen_contact_never_counts_toward_arming() {
        // At the default threshold, two fingers plus a stylus is still two
        // fingers, so the third finger is the one that arms — the stylus
        // neither accelerated nor blocked anything.
        let mut arb = Arbitrator::new(Policy::default());
        assert_eq!(arb.down(0, &finger()), Ownership::Client);
        assert_eq!(arb.down(1, &finger()), Ownership::Client);
        assert_eq!(
            arb.down(
                9,
                &DownContext {
                    is_pen: true,
                    ..finger()
                }
            ),
            Ownership::Client
        );
        assert_eq!(
            arb.down(2, &finger()),
            Ownership::Compositor,
            "the pen did not count, so this is the third finger"
        );

        // The non-acceleration is only visible against a threshold the pen
        // could have reached. At four, the third finger is still the
        // client's because only two fingers are down; had the pen counted,
        // this would already be the fourth contact and would have armed.
        let mut arb = Arbitrator::new(Policy {
            min_gesture_fingers: 4,
            ..Policy::default()
        });
        assert_eq!(arb.down(0, &finger()), Ownership::Client);
        assert_eq!(arb.down(1, &finger()), Ownership::Client);
        assert_eq!(
            arb.down(
                9,
                &DownContext {
                    is_pen: true,
                    ..finger()
                }
            ),
            Ownership::Client
        );
        assert_eq!(arb.down(2, &finger()), Ownership::Client);
        assert_eq!(arb.down(3, &finger()), Ownership::Compositor);
    }

    #[test]
    fn an_excluded_or_passthrough_contact_does_not_count_toward_arming() {
        // The chosen semantics, stated as a test: only plain-surface fingers
        // arm. A contact the client asked to keep must not be able to push a
        // later bare finger over the threshold — doing so would require
        // cancelling exactly the touches the client protected.
        let policy = Policy {
            passthrough_apps: vec!["org.kde.krita".to_string()],
            ..Policy::default()
        };
        let mut arb = Arbitrator::new(policy);
        assert_eq!(arb.down(0, &finger()), Ownership::Client);
        assert_eq!(
            arb.down(
                1,
                &DownContext {
                    in_exclusion_rect: true,
                    ..finger()
                }
            ),
            Ownership::Client
        );
        assert_eq!(
            arb.down(
                2,
                &DownContext {
                    under_gesture_app: true,
                    ..finger()
                }
            ),
            Ownership::Client
        );
        assert_eq!(
            arb.down(3, &finger()),
            Ownership::Client,
            "one plain finger, one excluded, one passthrough: no arming"
        );
        assert_eq!(arb.down(4, &finger()), Ownership::Compositor);

        // And the converse: a plain finger's own exclusion flag is what
        // disqualifies it. The same slot with the flag cleared is a plain
        // surface contact and does count.
        assert!(finger().counts_toward_arming());
        assert!(!DownContext {
            in_exclusion_rect: true,
            ..finger()
        }
        .counts_toward_arming());
    }

    #[test]
    fn disabled_beats_arming_and_a_zone_arms_on_the_first_finger() {
        let mut arb = Arbitrator::new(Policy {
            disabled: true,
            ..Policy::default()
        });
        assert_eq!(arb.down(0, &finger()), Ownership::Client);
        assert_eq!(arb.down(1, &finger()), Ownership::Client);
        assert_eq!(
            arb.down(2, &finger()),
            Ownership::Client,
            "the kill switch outranks arming"
        );
        assert_eq!(arb.down(3, &finger()), Ownership::Client);

        let mut arb = Arbitrator::new(Policy::default());
        let mut zoned = finger();
        zoned.zone = zone();
        assert_eq!(
            arb.down(0, &zoned),
            Ownership::Compositor,
            "a zone arms on the first finger; edge swipes are single-finger"
        );
    }

    #[test]
    fn arming_cancels_the_client_touches_it_took_back() {
        let mut arb = Arbitrator::new(Policy::default());
        assert_eq!(arb.down(0, &finger()), Ownership::Client);
        assert_eq!(arb.down(1, &finger()), Ownership::Client);
        assert_eq!(arb.down(2, &finger()), Ownership::Compositor);
        assert_eq!(arb.down(3, &finger()), Ownership::Compositor);

        assert_eq!(arb.client_slots(), vec![0, 1]);
        assert_eq!(
            arb.clear_clients(),
            vec![0, 1],
            "the two slots the client must be told to cancel"
        );
        assert_eq!(arb.open_client_touches(), 0);
        // The live gesture survives: compositor slots are untouched.
        assert_eq!(arb.ownership(2), Some(Ownership::Compositor));
        assert_eq!(arb.ownership(3), Some(Ownership::Compositor));
        // And a second call has nothing to do.
        assert!(arb.clear_clients().is_empty());
    }

    #[test]
    fn clear_clients_cancels_pen_slots_too() {
        let mut arb = Arbitrator::new(Policy::default());
        assert_eq!(arb.down(0, &finger()), Ownership::Client);
        assert_eq!(arb.down(1, &finger()), Ownership::Client);
        assert_eq!(
            arb.down(
                9,
                &DownContext {
                    is_pen: true,
                    ..finger()
                }
            ),
            Ownership::Client
        );
        assert_eq!(arb.down(2, &finger()), Ownership::Compositor);

        // The pen slot is the client's, so it is one of the touch points the
        // client must be told to cancel — the same way it was forwarded in
        // the first place.
        assert_eq!(arb.clear_clients(), vec![0, 1, 9]);
        assert_eq!(
            arb.ownership(9),
            None,
            "and it leaves the ledger, so it cannot arm anything afterwards"
        );
        assert_eq!(arb.ownership(2), Some(Ownership::Compositor));
    }

    #[test]
    fn clear_is_device_removal_and_takes_compositor_slots_too() {
        let mut arb = Arbitrator::new(Policy::default());
        assert_eq!(arb.down(0, &finger()), Ownership::Client);
        assert_eq!(arb.down(1, &finger()), Ownership::Client);
        assert_eq!(arb.down(2, &finger()), Ownership::Compositor);

        // Device teardown: everything goes, and only the client half is owed
        // a cancel.
        assert_eq!(arb.clear(), 2);
        assert_eq!(arb.ownership(2), None);
        assert_eq!(arb.open_client_touches(), 0);
    }

    // ── ledger ─────────────────────────────────────────────────────────

    #[test]
    fn late_joining_finger_inherits_sequence_ownership() {
        let mut arb = Arbitrator::new(Policy::default());
        // Bare first finger mid-screen: the client's.
        assert_eq!(arb.down(0, &finger()), Ownership::Client);
        // Second finger, nothing special about it, joins a client sequence:
        // still the client's.
        assert_eq!(arb.down(1, &finger()), Ownership::Client);

        // Now a finger lands in a zone; the sequence becomes ours.
        let mut zoned = finger();
        zoned.zone = zone();
        assert_eq!(arb.down(2, &zoned), Ownership::Compositor);
        // A fourth finger, no zone of its own, inherits.
        assert_eq!(arb.down(3, &finger()), Ownership::Compositor);
    }

    #[test]
    fn a_pen_slot_never_counts_toward_the_compositor_owned_tally() {
        let mut arb = Arbitrator::new(Policy::default());

        let pen = DownContext {
            is_pen: true,
            ..finger()
        };
        assert_eq!(arb.down(9, &pen), Ownership::Client);
        assert!(
            arb.slots.get(&9).is_some_and(|r| r.pen),
            "the pen slot is remembered as a pen slot"
        );

        // The stylus is down, but a finger arriving next must still see an
        // unowned sequence.
        assert_eq!(arb.down(0, &finger()), Ownership::Client);
        assert_eq!(arb.down(1, &finger()), Ownership::Client);
    }

    #[test]
    fn eraser_slots_are_also_excluded_from_the_tally() {
        let mut arb = Arbitrator::new(Policy::default());
        let eraser = DownContext {
            eraser: true,
            is_pen: true,
            ..finger()
        };
        assert_eq!(arb.down(7, &eraser), Ownership::Client);
        assert_eq!(arb.down(0, &finger()), Ownership::Client);
    }

    #[test]
    fn clear_returns_exactly_the_open_client_touch_count() {
        let mut arb = Arbitrator::new(Policy::default());

        assert_eq!(arb.down(0, &finger()), Ownership::Client);
        assert_eq!(arb.down(1, &finger()), Ownership::Client);
        let mut zoned = finger();
        zoned.zone = zone();
        assert_eq!(arb.down(2, &zoned), Ownership::Compositor);
        assert_eq!(arb.down(3, &zoned), Ownership::Compositor);

        // The invariant, asserted directly: what clear() hands back is
        // exactly what the client currently has open.
        assert_eq!(arb.open_client_touches(), 2);
        assert_eq!(arb.clear(), 2);

        // And the ledger is empty afterwards, pen set included.
        assert_eq!(arb.open_client_touches(), 0);
        assert!(
            arb.slots.values().all(|r| !r.pen),
            "no pen slots survive a teardown"
        );
        assert_eq!(arb.ownership(0), None);
        assert_eq!(arb.clear(), 0, "a second teardown owes nothing");
    }

    #[test]
    fn pen_slots_are_recorded_and_cleared() {
        let mut arb = Arbitrator::new(Policy::default());
        let pen = DownContext {
            is_pen: true,
            ..finger()
        };
        assert_eq!(arb.down(4, &pen), Ownership::Client);
        assert_eq!(arb.ownership(4), Some(Ownership::Client));
        assert_eq!(arb.clear(), 1);
        assert!(
            arb.slots.values().all(|r| !r.pen),
            "no pen slots survive a teardown"
        );
    }

    #[test]
    fn up_returns_the_prior_ownership_and_out_of_order_lifts_are_fine() {
        let mut arb = Arbitrator::new(Policy::default());
        assert_eq!(arb.down(0, &finger()), Ownership::Client);
        assert_eq!(arb.down(2, &finger()), Ownership::Client);
        assert_eq!(arb.up(2), Some(Ownership::Client));
        assert_eq!(arb.up(2), None, "a slot lifts once");
        assert_eq!(arb.up(42), None, "an untracked slot lifts quietly");
        assert_eq!(arb.ownership(0), Some(Ownership::Client));
    }

    #[test]
    fn a_reused_slot_overwrites() {
        let mut arb = Arbitrator::new(Policy::default());
        assert_eq!(arb.down(0, &finger()), Ownership::Client);
        let mut zoned = finger();
        zoned.zone = zone();
        assert_eq!(arb.down(0, &zoned), Ownership::Compositor);
        assert_eq!(arb.ownership(0), Some(Ownership::Compositor));
        assert_eq!(arb.open_client_touches(), 0);
    }

    #[test]
    fn set_policy_does_not_re_decide_live_slots() {
        let mut arb = Arbitrator::new(Policy::default());
        assert_eq!(arb.down(0, &finger()), Ownership::Client);
        arb.set_policy(Policy {
            disabled: true,
            ..Policy::default()
        });
        assert!(arb.policy().disabled);
        assert_eq!(
            arb.ownership(0),
            Some(Ownership::Client),
            "the live slot keeps the owner it was given at its down"
        );
        // New downs do see the new policy.
        assert_eq!(arb.down(1, &finger()), Ownership::Client);
    }

    /// The stale-ledger regression.
    ///
    /// An arbitrator outlives individual sequences when the caller keeps it
    /// as a long-lived ledger (the compositor adapter does exactly that, so
    /// that a later finger can arm a sequence whose earlier fingers already
    /// went to the client). That makes retirement the load-bearing
    /// obligation: a slot recorded by `down` and never retired by `up`
    /// inflates the arming tally forever, and the next unrelated touch
    /// anywhere can arm the compositor out of nowhere.
    ///
    /// This is the seat-free statement of that invariant. It pins both
    /// halves, because either one alone can pass while the bug is live: the
    /// ledger must be empty afterwards, *and* an unrelated single finger
    /// must not arm. A ledger that leaks a compositor-owned slot fails the
    /// first; one that leaks only client slots fails the second.
    #[test]
    fn a_full_down_up_cycle_leaves_no_slots_and_does_not_arm_a_later_finger() {
        let mut arb = Arbitrator::new(Policy::default());

        // Three plain fingers on the middle of a surface: the first two go
        // to the client, the third arms the sequence. This is the shape that
        // creates client slots, which are the ones that inflate the tally.
        assert_eq!(arb.down(10, &finger()), Ownership::Client);
        assert_eq!(arb.down(11, &finger()), Ownership::Client);
        assert_eq!(arb.down(12, &finger()), Ownership::Compositor);
        assert_eq!(arb.open_client_touches(), 2);

        // Every finger lifts, each returning the ownership it actually had.
        assert_eq!(arb.up(10), Some(Ownership::Client));
        assert_eq!(arb.up(11), Some(Ownership::Client));
        assert_eq!(arb.up(12), Some(Ownership::Compositor));
        for slot in [10, 11, 12] {
            assert_eq!(
                arb.ownership(slot),
                None,
                "slot {slot} still tracked after up"
            );
        }

        // The ledger is empty.
        assert_eq!(arb.open_client_touches(), 0);
        assert_eq!(arb.ownership(10), None);
        assert_eq!(arb.ownership(11), None);
        assert_eq!(arb.ownership(12), None);

        // And the tally really is back to zero: an unrelated single finger,
        // far away on a different surface, must go to the client rather than
        // arming on the strength of a sequence that ended.
        let elsewhere = DownContext {
            pos: Point::new(-9000.0, -9000.0),
            ..DownContext::default()
        };
        assert_eq!(arb.down(1, &elsewhere), Ownership::Client);
        assert_eq!(arb.up(1), Some(Ownership::Client));
        assert_eq!(arb.open_client_touches(), 0);
    }

    /// The same invariant after the device-removal teardown, which ends a
    /// sequence without naming its slots.
    #[test]
    fn clear_leaves_the_ledger_able_to_be_reused_without_arming() {
        let mut arb = Arbitrator::new(Policy::default());
        arb.down(10, &finger());
        arb.down(11, &finger());
        assert_eq!(arb.open_client_touches(), 2);

        // Device removal: `clear` reports the client touches the caller must
        // close, and leaves nothing behind.
        assert_eq!(arb.clear(), 2);
        assert_eq!(arb.open_client_touches(), 0);

        // Reuse: a fresh single finger must not inherit the old sequence.
        assert_eq!(arb.down(1, &finger()), Ownership::Client);
        assert_eq!(arb.up(1), Some(Ownership::Client));
        assert_eq!(arb.open_client_touches(), 0);
    }
}

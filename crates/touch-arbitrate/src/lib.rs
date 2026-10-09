//! `touch-arbitrate` — layer L3 of the niri-toublet touch stack.
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

use std::collections::{BTreeMap, BTreeSet};

use touch_core::Zone;

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
#[derive(Debug, Clone, Default, PartialEq)]
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

/// The single ownership transition function.
///
/// # Precedence
///
/// The order below is the contract. It is not an implementation detail and
/// it is not free to change: it mirrors niri-touch's `touch_gesture.rs`
/// lines 274–284, where edge zones and Mod are tested before the
/// passthrough-app rule, and it layers the pen/eraser/tool rules and the
/// rect exclusion on top in that same "tool identity first, Mod second,
/// passthrough last" shape.
///
/// 1. [`Policy::disabled`] → [`Ownership::Client`]. The kill switch outranks
///    everything, including Mod and zones. Touch must never be able to wedge
///    a session, so the escape hatch has to outrank the escape hatches.
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
///    escape hatch for "this app swallowed my gestures".
/// 8. Otherwise, fall through to sequence ownership:
///    [`Ownership::Compositor`] if the sequence already has a
///    compositor-owned contact, else [`Ownership::Client`].
///
/// Step 8 is the documented last case, and the chain is exhaustive in the
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
pub fn decide(ctx: &DownContext, policy: &Policy, compositor_owned_in_sequence: bool) -> Ownership {
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
    // 7. A zone is the compositor's territory.
    if ctx.zone.is_some() {
        return Ownership::Compositor;
    }
    // 8. Documented fallthrough: inherit sequence ownership.
    if compositor_owned_in_sequence {
        Ownership::Compositor
    } else {
        Ownership::Client
    }
}

/// The per-seat ownership ledger.
///
/// [`decide`] is the rule; this is the bookkeeping the rule needs. It
/// tracks which slots are down and who owns each, so that
/// sequence ownership (step 8 of [`decide`]) and device-removal teardown
/// work without a live seat — the two things niri-tablet needed patches
/// `0010` and `0012` for.
///
/// # The invariant
///
/// **A client-visible touch is never left open when a gesture grab
/// installs.** [`clear`] returns the number of slots it dropped that the
/// client had been given a down for; the caller must close exactly that many
/// client-visible touch points. Skipping the close leaves the phantom
/// touches niri-tablet patch `0011` describes — a browser pinch-zooming from
/// one finger and ignoring every later tap, up to nine phantoms at once.
/// The count is the contract, not a comment: it is returned, it is
/// computable without a seat via [`Arbitrator::open_client_touches`], and it
/// is asserted in tests.
#[derive(Debug, Clone)]
pub struct Arbitrator {
    policy: Policy,
    slots: BTreeMap<u32, Ownership>,
    /// Slots that came from a pen or eraser.
    ///
    /// Tracked separately because a pen contact is *always* the client's (see
    /// [`decide`] steps 2 and 3). If such a slot counted toward the
    /// compositor-owned tally in step 8, a resting stylus on the glass would
    /// silently arm the compositor's gesture recogniser for the next finger —
    /// a bug that would look like random gesture activation.
    pen_slots: BTreeSet<u32>,
}

impl Arbitrator {
    /// A new arbitrator with an empty ledger.
    pub fn new(policy: Policy) -> Self {
        Self {
            policy,
            slots: BTreeMap::new(),
            pen_slots: BTreeSet::new(),
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
    pub fn down(&mut self, slot: u32, ctx: &DownContext) -> Ownership {
        let compositor_owned_in_sequence = self
            .slots
            .iter()
            .filter(|(s, _)| !self.pen_slots.contains(s))
            .any(|(_, o)| *o == Ownership::Compositor);

        let ownership = decide(ctx, &self.policy, compositor_owned_in_sequence);

        if ctx.is_pen || ctx.eraser {
            self.pen_slots.insert(slot);
        }
        self.slots.insert(slot, ownership);

        ownership
    }

    /// Record a touch-up and return the ownership it had, if it was tracked.
    ///
    /// Out-of-order lifts are normal and must not be errors: a caller that
    /// gets `None` here has an untracked slot, which is a desync worth
    /// logging but not worth asserting on mid-gesture.
    pub fn up(&mut self, slot: u32) -> Option<Ownership> {
        self.pen_slots.remove(&slot);
        self.slots.remove(&slot)
    }

    /// The recorded ownership of `slot`, if it is currently down.
    pub fn ownership(&self, slot: u32) -> Option<Ownership> {
        self.slots.get(&slot).copied()
    }

    /// Drop every slot, returning how many client-visible touches the caller
    /// must close.
    ///
    /// This is the device-removal / gesture-grab path: `touch_end` in
    /// `wl_touch` terms, or the teardown when a gesture grab installs over
    /// contacts the client already has. See the type-level invariant — the
    /// returned count is exactly the number of client-owned slots dropped,
    /// and the caller owes exactly that many touch-point closes.
    pub fn clear(&mut self) -> usize {
        let closed = self.open_client_touches();
        self.slots.clear();
        self.pen_slots.clear();
        closed
    }

    /// How many client-visible touches are currently open.
    ///
    /// Exposed so callers — and tests — can assert the teardown invariant
    /// without having to call [`clear`] first.
    pub fn open_client_touches(&self) -> usize {
        self.slots
            .iter()
            .filter(|(_, o)| **o == Ownership::Client)
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
        assert_eq!(decide(&ctx, &policy, false), Ownership::Client);
        assert_eq!(decide(&ctx, &policy, true), Ownership::Client);
    }

    #[test]
    fn pen_without_mod_is_the_clients() {
        let ctx = DownContext {
            is_pen: true,
            ..finger()
        };
        assert_eq!(decide(&ctx, &Policy::default(), false), Ownership::Client);
        // Even mid-sequence, and even in a zone.
        assert_eq!(
            decide(
                &DownContext {
                    zone: zone(),
                    ..ctx
                },
                &Policy::default(),
                true
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
            decide(&ctx, &Policy::default(), false),
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
        assert_eq!(decide(&ctx, &Policy::default(), false), Ownership::Client);
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
            decide(&ctx, &policy, false),
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
        assert_eq!(decide(&ctx, &policy, false), Ownership::Client);
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
            decide(&ctx, &policy, false),
            Ownership::Compositor,
            "edge swipes must stay reachable from a passthrough app"
        );

        // The same app, same spot, no zone: now it is the client's.
        let ctx = DownContext {
            under_gesture_app: true,
            zone: None,
            ..finger()
        };
        assert_eq!(decide(&ctx, &policy, false), Ownership::Client);
    }

    #[test]
    fn sequence_ownership_is_the_documented_fallthrough() {
        // Nothing special about the contact: bare first finger, then a second.
        assert_eq!(
            decide(&finger(), &Policy::default(), false),
            Ownership::Client
        );
        assert_eq!(
            decide(&finger(), &Policy::default(), true),
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
        assert!(arb.pen_slots.contains(&9));

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
        assert!(arb.pen_slots.is_empty());
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
        assert!(arb.pen_slots.is_empty());
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
}

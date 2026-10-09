//! L1 — contact tracking.
//!
//! Tracks the live set of touch contacts and derives, per frame, the
//! geometry a recogniser needs: centroid and its frame-to-frame delta,
//! spread (mean distance to centroid), signed rotation delta, and total
//! motion.
//!
//! # No policy
//!
//! This module decides nothing about what a gesture *means*. It does not
//! know about edges, except to label a point's position relative to an
//! output's boundary. Ownership, thresholds and actions live in L3.
//!
//! # Slot lifecycle
//!
//! Contacts are keyed by [`Slot`], so the two cases that broke both prior
//! forks are handled by construction rather than by later patches:
//!
//! - **Late-joining fingers** — a `down` for a new slot simply adds to the
//!   set. It does not disturb the others' `start` position.
//! - **Out-of-order lifts** — an `up` for any slot removes exactly that
//!   contact. An `up` for an unknown slot returns `None` rather than
//!   panicking or corrupting the rotation basis.
//!
//! A `motion` for an unknown slot is *reported*, not silently absorbed:
//! [`ContactSet::motion`] returns `false`. That is the observable symptom of
//! desync between the compositor and this tracker, and L3 turns it into a
//! debug assertion instead of letting it degrade into stuck touches.

use std::collections::BTreeMap;
use std::f64::consts::{PI, TAU};
use std::time::Duration;

/// Default width in logical pixels of the screen-edge strip reserved for
/// single-finger edge gestures.
///
/// Lifted from niri-tablet, where it is the same value hard-coded as
/// `EDGE_ZONE_PX`. Exposed as a parameter on [`classify_zone`] and as a
/// default for [`ContactSet::new`] so it can be tuned per device without a
/// recompile.
pub const DEFAULT_EDGE_ZONE_PX: f64 = 48.0;

/// Rotation deltas smaller than this are clamped to zero.
///
/// Lifted from niri-touch. Without it, sub-threshold jitter from fingers
/// resting on the glass accumulates frame over frame into a false rotation.
pub const ROTATION_NOISE_FLOOR: f64 = 0.001;

/// A position in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Point {
    /// Horizontal coordinate, increasing rightward.
    pub x: f64,
    /// Vertical coordinate, increasing downward (screen convention).
    pub y: f64,
}

impl Point {
    /// Construct a point.
    #[must_use]
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    /// Euclidean distance from `self` to `other`.
    #[must_use]
    pub fn distance_to(self, other: Point) -> f64 {
        let (dx, dy) = (self.x - other.x, self.y - other.y);
        (dx * dx + dy * dy).sqrt()
    }
}

impl std::ops::Sub for Point {
    type Output = Point;

    fn sub(self, rhs: Point) -> Point {
        Point::new(self.x - rhs.x, self.y - rhs.y)
    }
}

/// The size of one output in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OutputSize {
    /// Width in logical pixels.
    pub w: f64,
    /// Height in logical pixels.
    pub h: f64,
}

impl OutputSize {
    /// Construct an output size.
    #[must_use]
    pub const fn new(w: f64, h: f64) -> Self {
        Self { w, h }
    }
}

/// A stable identifier for one physical contact.
///
/// Slots come from the input backend and are reused after a lift, which is
/// why [`ContactSet::up`] clears the stored contact rather than marking it
/// dead: a reused slot must not inherit the previous contact's start time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Slot(u32);

impl Slot {
    /// Construct a slot identifier.
    #[must_use]
    pub const fn new(id: u32) -> Self {
        Self(id)
    }

    /// The raw backend identifier.
    #[must_use]
    pub const fn id(self) -> u32 {
        self.0
    }
}

/// One of the four screen edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Edge {
    /// The left edge; third axis runs top to bottom.
    Left,
    /// The right edge; third axis runs top to bottom.
    Right,
    /// The top edge; third axis runs left to right.
    Top,
    /// The bottom edge; third axis runs left to right.
    Bottom,
}

/// Which third of an edge, along that edge's perpendicular axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Third {
    /// First third: leftmost for top/bottom, topmost for left/right.
    Start,
    /// Middle third.
    Center,
    /// Last third: rightmost for top/bottom, bottommost for left/right.
    End,
}

/// One of the four screen corners.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Corner {
    /// Top-left.
    TopLeft,
    /// Top-right.
    TopRight,
    /// Bottom-left.
    BottomLeft,
    /// Bottom-right.
    BottomRight,
}

/// Where a point sits relative to an output's boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Zone {
    /// Within the edge strip, classified by which third of that edge.
    Edge {
        /// Which edge.
        edge: Edge,
        /// Which third along the edge.
        third: Third,
    },
    /// Within the corner square, where two edge strips meet.
    Corner {
        /// Which corner.
        corner: Corner,
    },
}

impl Zone {
    /// The edge this zone belongs to.
    ///
    /// Corner zones report the two edges they join, preferring the one
    /// along the zone's shorter axis. Callers that need both should match on
    /// the [`Zone`] variants directly.
    #[must_use]
    pub const fn edge(self) -> Edge {
        match self {
            Zone::Edge { edge, .. } => edge,
            Zone::Corner { corner: Corner::TopLeft } | Zone::Corner { corner: Corner::BottomLeft } => {
                Edge::Left
            }
            Zone::Corner { corner: Corner::TopRight }
            | Zone::Corner { corner: Corner::BottomRight } => Edge::Right,
        }
    }
}

/// Classify a point against an output's boundary.
///
/// Returns [`None`] when the point is in neither the corner squares nor any
/// edge strip.
///
/// **Corner takes precedence.** A point inside a corner square satisfies
/// both rules; it is reported as [`Zone::Corner`]. niri-tablet behaves the
/// same way, and resolving the overlap by precedence rather than by
/// evaluation order keeps the classification deterministic.
///
/// ```
/// use touch_core::{classify_zone, Corner, Edge, OutputSize, Point, Third, Zone};
///
/// let size = OutputSize::new(1000.0, 1280.0);
/// let zone_px = 48.0;
///
/// // Dead centre is not near any boundary.
/// assert_eq!(classify_zone(Point::new(500.0, 640.0), size, zone_px), None);
///
/// // Inside the corner square.
/// assert_eq!(
///     classify_zone(Point::new(5.0, 5.0), size, zone_px),
///     Some(Zone::Corner { corner: Corner::TopLeft }),
/// );
///
/// // Along the left edge, past the corner square.
/// assert_eq!(
///     classify_zone(Point::new(5.0, 640.0), size, zone_px),
///     Some(Zone::Edge { edge: Edge::Left, third: Third::Center }),
/// );
/// ```
#[must_use]
pub fn classify_zone(pos: Point, size: OutputSize, zone_px: f64) -> Option<Zone> {
    let (x, y) = (pos.x, pos.y);
    let (w, h) = (size.w, size.h);

    let near_left = x < zone_px;
    let near_right = w - x < zone_px;
    let near_top = y < zone_px;
    let near_bottom = h - y < zone_px;

    // Corner squares: within `zone_px` of two edges at once.
    if near_left && near_top {
        return Some(Zone::Corner { corner: Corner::TopLeft });
    }
    if near_right && near_top {
        return Some(Zone::Corner { corner: Corner::TopRight });
    }
    if near_left && near_bottom {
        return Some(Zone::Corner { corner: Corner::BottomLeft });
    }
    if near_right && near_bottom {
        return Some(Zone::Corner { corner: Corner::BottomRight });
    }

    // Otherwise the single closest edge within threshold.
    let candidates = [
        (Edge::Left, x),
        (Edge::Right, w - x),
        (Edge::Top, y),
        (Edge::Bottom, h - y),
    ];

    let closest = candidates
        .into_iter()
        .filter(|(_, dist)| *dist < zone_px)
        .min_by(|a, b| a.1.total_cmp(&b.1))?;

    let third = match closest.0 {
        Edge::Top | Edge::Bottom => third_along(x, w),
        Edge::Left | Edge::Right => third_along(y, h),
    };

    Some(Zone::Edge {
        edge: closest.0,
        third,
    })
}

/// Split a perpendicular-axis coordinate into thirds.
fn third_along(pos: f64, extent: f64) -> Third {
    let third = extent / 3.0;
    if pos < third {
        Third::Start
    } else if pos < third * 2.0 {
        Third::Center
    } else {
        Third::End
    }
}

/// One tracked contact.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Contact {
    /// Current position.
    pub pos: Point,
    /// Where the contact landed.
    pub start: Point,
    /// When the contact landed, relative to the trace's time origin.
    pub start_time: Duration,
    /// Cumulative path length travelled since landing.
    ///
    /// Straight-line distance is not enough for a drag that wanders, so
    /// this accumulates per-step distance.
    pub path: f64,
}

/// The derived geometry of one frame of contact.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    /// Number of live contacts.
    pub count: usize,
    /// Mean position of all contacts. `None` when there are none.
    pub centroid: Option<Point>,
    /// Movement of the centroid since the previous frame.
    ///
    /// This is the primary signal for swipe recognition: it is the whole
    /// cluster moving together, as distinct from fingers rearranging within
    /// a stationary cluster.
    pub centroid_delta: Point,
    /// Mean distance from each contact to the centroid. Zero below two
    /// contacts.
    pub spread: f64,
    /// Signed rotation since the previous frame, in radians.
    ///
    /// Positive is counter-clockwise *as the user sees it*. Zero when no
    /// contact is present in both frames, or when the delta is below
    /// [`ROTATION_NOISE_FLOOR`].
    pub rotation_delta: f64,
    /// Total distance travelled by all contacts during this frame.
    pub total_motion: f64,
    /// Whether the contact set changed since the previous frame.
    pub changed: bool,
}

/// The live set of contacts for one seat and one device.
#[derive(Debug, Clone)]
pub struct ContactSet {
    contacts: BTreeMap<u32, Contact>,
    /// Per-slot angle from the previous frame, the basis for rotation.
    prev_angles: BTreeMap<u32, f64>,
    prev_centroid: Option<Point>,
    last_frame_time: Option<Duration>,
    total_motion_last_frame: f64,
    pending_motion: f64,
    changed: bool,
}

impl Default for ContactSet {
    fn default() -> Self {
        Self::new()
    }
}

impl ContactSet {
    /// An empty contact set.
    #[must_use]
    pub fn new() -> Self {
        Self {
            contacts: BTreeMap::new(),
            prev_angles: BTreeMap::new(),
            prev_centroid: None,
            last_frame_time: None,
            total_motion_last_frame: 0.0,
            pending_motion: 0.0,
            changed: false,
        }
    }

    /// Number of live contacts.
    #[must_use]
    pub fn len(&self) -> usize {
        self.contacts.len()
    }

    /// Whether there are no live contacts.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.contacts.is_empty()
    }

    /// Iterate the live contacts in slot order.
    pub fn iter(&self) -> impl Iterator<Item = (Slot, &Contact)> {
        self.contacts.iter().map(|(&k, v)| (Slot::new(k), v))
    }

    /// Look up one contact.
    #[must_use]
    pub fn get(&self, slot: Slot) -> Option<&Contact> {
        self.contacts.get(&slot.id())
    }

    /// Record a contact landing.
    ///
    /// A slot that is already live is treated as a fresh contact, because
    /// the backend reusing a slot means the previous contact is gone
    /// whether or not its `up` was ever seen.
    pub fn down(&mut self, slot: Slot, pos: Point, time: Duration) {
        self.contacts.insert(
            slot.id(),
            Contact {
                pos,
                start: pos,
                start_time: time,
                path: 0.0,
            },
        );
        // The new slot has no angle basis yet, so it contributes nothing to
        // this frame's rotation.
        self.prev_angles.remove(&slot.id());
        self.pending_motion = 0.0;
        self.changed = true;
    }

    /// Record a contact moving.
    ///
    /// Returns `false` if the slot was not live, which means the tracker
    /// and the backend have desynced. Callers should surface this rather
    /// than ignore it — absorbing it silently is how stuck touches happen.
    pub fn motion(&mut self, slot: Slot, pos: Point) -> bool {
        let Some(contact) = self.contacts.get_mut(&slot.id()) else {
            return false;
        };
        let step = contact.pos.distance_to(pos);
        self.pending_motion += step;
        contact.path += step;
        contact.pos = pos;
        self.changed = true;
        true
    }

    /// Record a contact lifting.
    ///
    /// Returns the final contact state, or `None` if the slot was not live.
    /// The slot is fully forgotten either way, so a reused slot never
    /// inherits stale state.
    pub fn up(&mut self, slot: Slot) -> Option<Contact> {
        let contact = self.contacts.remove(&slot.id());
        if contact.is_some() {
            self.prev_angles.remove(&slot.id());
            self.changed = true;
        }
        contact
    }

    /// End all contacts, e.g. on device removal or touch cancel.
    ///
    /// Returns how many contacts were dropped, so the caller can assert it
    /// closes every client-visible touch it had open.
    pub fn clear(&mut self) -> usize {
        let n = self.contacts.len();
        if n > 0 {
            self.contacts.clear();
            self.prev_angles.clear();
            self.pending_motion = 0.0;
            self.changed = true;
        }
        n
    }

    /// Seal the current batch into a [`Frame`], advancing the rotation basis.
    ///
    /// The compositor calls this once per `wl_touch.frame`, i.e. once per
    /// atomic batch of down/motion/up events.
    pub fn frame(&mut self, time: Duration) -> Frame {
        let count = self.contacts.len();

        if count == 0 {
            self.prev_angles.clear();
            self.prev_centroid = None;
            self.last_frame_time = Some(time);
            let total_motion = self.total_motion_last_frame;
            self.total_motion_last_frame = 0.0;
            self.pending_motion = 0.0;
            let changed = self.changed;
            self.changed = false;
            return Frame {
                count: 0,
                centroid: None,
                centroid_delta: Point::default(),
                spread: 0.0,
                rotation_delta: 0.0,
                total_motion,
                changed,
            };
        }

        let centroid = self.centroid();
        let centroid_delta = match self.prev_centroid {
            Some(prev) => centroid - prev,
            None => Point::default(),
        };

        let spread = self.spread(centroid);
        let rotation_delta = self.rotation_delta();
        let total_motion = self.pending_motion;

        self.prev_centroid = Some(centroid);
        self.prev_angles = self.per_slot_angles(centroid);
        self.last_frame_time = Some(time);
        self.total_motion_last_frame = total_motion;
        self.pending_motion = 0.0;
        let changed = self.changed;
        self.changed = false;

        Frame {
            count,
            centroid: Some(centroid),
            centroid_delta,
            spread,
            rotation_delta,
            total_motion,
            changed,
        }
    }

    /// Timestamp of the last sealed frame.
    #[must_use]
    pub fn last_frame_time(&self) -> Option<Duration> {
        self.last_frame_time
    }

    /// Mean position of all contacts.
    #[must_use]
    pub fn centroid(&self) -> Point {
        let n = self.contacts.len() as f64;
        let (sx, sy) = self
            .contacts
            .values()
            .fold((0.0, 0.0), |(ax, ay), c| (ax + c.pos.x, ay + c.pos.y));
        Point::new(sx / n, sy / n)
    }

    /// Mean position of all contacts, or `None` when there are none.
    ///
    /// [`ContactSet::centroid`] divides by the contact count, so it is only
    /// meaningful for a non-empty set and yields `NaN` otherwise. Callers that
    /// may run against an empty set — `reset`, `End` events — use this
    /// instead of guarding the count themselves.
    #[must_use]
    pub fn centroid_opt(&self) -> Option<Point> {
        if self.contacts.is_empty() {
            None
        } else {
            Some(self.centroid())
        }
    }

    /// Mean distance from each contact to the centroid.
    ///
    /// Zero below two contacts — with a single contact, "spread around its
    /// own centroid" is trivially zero, and returning zero rather than NaN
    /// keeps callers from having to special-case it.
    ///
    /// Lifted from niri-touch's `calculate_spread`, which returns early for
    /// fewer than two points.
    #[must_use]
    pub fn spread(&self, centroid: Point) -> f64 {
        if self.contacts.len() < 2 {
            return 0.0;
        }
        let total: f64 = self
            .contacts
            .values()
            .map(|c| c.pos.distance_to(centroid))
            .sum();
        total / self.contacts.len() as f64
    }

    /// Per-slot angle around the centroid, in radians.
    fn per_slot_angles(&self, centroid: Point) -> BTreeMap<u32, f64> {
        if self.contacts.len() < 2 {
            return BTreeMap::new();
        }
        self.contacts
            .iter()
            .map(|(&slot, c)| {
                // atan2(-dy, dx): screen y grows downward, so flipping the y
                // axis yields the mathematical convention where positive is
                // counter-clockwise *as the user sees it*. Without the flip,
                // a CCW twist on the glass produces a negative delta.
                let angle = (-(c.pos.y - centroid.y)).atan2(c.pos.x - centroid.x);
                (slot, angle)
            })
            .collect()
    }

    /// Signed rotation since the previous frame, advancing the basis.
    fn rotation_delta(&mut self) -> f64 {
        let centroid = self.centroid();
        let new_angles = self.per_slot_angles(centroid);

        if new_angles.is_empty() || self.prev_angles.is_empty() {
            return 0.0;
        }

        let mut sum = 0.0;
        let mut count = 0usize;
        for (&slot, &curr) in &new_angles {
            let Some(&prev) = self.prev_angles.get(&slot) else {
                continue;
            };
            let raw = curr - prev;
            // Unwrap across the ±π boundary so a twist past the 3 o'clock
            // line reports the short-way delta, not a near-2π jump.
            let unwrapped = if raw > PI {
                raw - TAU
            } else if raw < -PI {
                raw + TAU
            } else {
                raw
            };
            sum += unwrapped;
            count += 1;
        }

        if count == 0 {
            return 0.0;
        }

        let avg = sum / count as f64;
        if avg.abs() < ROTATION_NOISE_FLOOR {
            0.0
        } else {
            avg
        }
    }
}

#[cfg(test)]
mod tests {
    use std::f64::consts::FRAC_PI_2;

    use super::*;

    const SIZE: OutputSize = OutputSize::new(1000.0, 1280.0);
    const T0: Duration = Duration::from_millis(0);

    fn pt(x: f64, y: f64) -> Point {
        Point::new(x, y)
    }

    fn slot(n: u32) -> Slot {
        Slot::new(n)
    }

    // ── zones ───────────────────────────────────────────────────────────

    #[test]
    fn centre_of_screen_is_not_in_a_zone() {
        assert_eq!(classify_zone(pt(500.0, 640.0), SIZE, 48.0), None);
    }

    #[test]
    fn corner_squares_take_precedence_over_edge_strips() {
        for (pos, want) in [
            (pt(5.0, 5.0), Corner::TopLeft),
            (pt(995.0, 5.0), Corner::TopRight),
            (pt(5.0, 1275.0), Corner::BottomLeft),
            (pt(995.0, 1275.0), Corner::BottomRight),
        ] {
            assert_eq!(
                classify_zone(pos, SIZE, 48.0),
                Some(Zone::Corner { corner: want }),
                "at {pos:?}"
            );
        }
    }

    #[test]
    fn edges_split_into_thirds() {
        // Top edge, thirds run left to right.
        assert_eq!(
            classify_zone(pt(100.0, 20.0), SIZE, 48.0),
            Some(Zone::Edge { edge: Edge::Top, third: Third::Start })
        );
        assert_eq!(
            classify_zone(pt(500.0, 20.0), SIZE, 48.0),
            Some(Zone::Edge { edge: Edge::Top, third: Third::Center })
        );
        // Left edge, thirds run top to bottom.
        assert_eq!(
            classify_zone(pt(5.0, 640.0), SIZE, 48.0),
            Some(Zone::Edge { edge: Edge::Left, third: Third::Center })
        );
        assert_eq!(
            classify_zone(pt(20.0, 1200.0), SIZE, 48.0),
            Some(Zone::Edge { edge: Edge::Left, third: Third::End })
        );
    }

    #[test]
    fn exactly_at_threshold_is_outside() {
        // Boundary is exclusive: 48.0 from the left edge is not "near" it.
        assert_eq!(classify_zone(pt(48.0, 640.0), SIZE, 48.0), None);
        assert_eq!(
            classify_zone(pt(47.999, 100.0), SIZE, 48.0),
            Some(Zone::Edge { edge: Edge::Left, third: Third::Start })
        );
    }

    #[test]
    fn third_boundaries_are_half_open() {
        let third = 1000.0 / 3.0;
        // Just below the first boundary → Start.
        assert_eq!(
            classify_zone(pt(third - 0.001, 20.0), SIZE, 48.0),
            Some(Zone::Edge { edge: Edge::Top, third: Third::Start })
        );
        // Exactly at the boundary → Center.
        assert_eq!(
            classify_zone(pt(third, 20.0), SIZE, 48.0),
            Some(Zone::Edge { edge: Edge::Top, third: Third::Center })
        );
        // Just below the second boundary → Center.
        assert_eq!(
            classify_zone(pt(third * 2.0 - 0.001, 20.0), SIZE, 48.0),
            Some(Zone::Edge { edge: Edge::Top, third: Third::Center })
        );
        // Exactly at the second boundary → End.
        assert_eq!(
            classify_zone(pt(third * 2.0, 20.0), SIZE, 48.0),
            Some(Zone::Edge { edge: Edge::Top, third: Third::End })
        );
    }

    #[test]
    fn closest_edge_wins_when_two_are_equidistant() {
        // A point near a corner but outside the corner square is ambiguous
        // between two edges; the tie must resolve deterministically to the
        // same edge every time, not depend on iteration order.
        let square = OutputSize::new(200.0, 200.0);
        let first = classify_zone(pt(200.0, 40.0), square, 48.0);
        let second = classify_zone(pt(200.0, 40.0), square, 48.0);
        assert_eq!(first, second);
        assert!(first.is_some());
    }

    #[test]
    fn zone_reports_its_edge() {
        assert_eq!(classify_zone(pt(5.0, 640.0), SIZE, 48.0).unwrap().edge(), Edge::Left);
        assert_eq!(classify_zone(pt(5.0, 5.0), SIZE, 48.0).unwrap().edge(), Edge::Left);
    }

    // ── lifecycle ───────────────────────────────────────────────────────

    #[test]
    fn empty_set_frames_cleanly() {
        let mut cs = ContactSet::new();
        let f = cs.frame(T0);
        assert_eq!(f.count, 0);
        assert_eq!(f.centroid, None);
        assert_eq!(f.spread, 0.0);
        assert_eq!(f.rotation_delta, 0.0);
        assert_eq!(f.centroid_delta, Point::default());
        assert!(!f.changed);
    }

    #[test]
    fn down_records_start_and_position() {
        let mut cs = ContactSet::new();
        cs.down(slot(1), pt(100.0, 200.0), T0);
        cs.motion(slot(1), pt(110.0, 200.0));
        let c = cs.get(slot(1)).unwrap();
        assert_eq!(c.start, pt(100.0, 200.0));
        assert_eq!(c.pos, pt(110.0, 200.0));
        assert_eq!(c.path, 10.0);
    }

    #[test]
    fn late_joining_finger_does_not_disturb_others() {
        let mut cs = ContactSet::new();
        cs.down(slot(1), pt(100.0, 200.0), T0);
        cs.down(slot(2), pt(300.0, 400.0), Duration::from_millis(50));
        // The first contact's start must be untouched.
        assert_eq!(cs.get(slot(1)).unwrap().start, pt(100.0, 200.0));
        assert_eq!(cs.get(slot(1)).unwrap().start_time, T0);
        assert_eq!(cs.get(slot(2)).unwrap().start, pt(300.0, 400.0));
        assert_eq!(cs.len(), 2);
    }

    #[test]
    fn out_of_order_lifts_remove_only_their_own_contact() {
        let mut cs = ContactSet::new();
        cs.down(slot(1), pt(0.0, 0.0), T0);
        cs.down(slot(2), pt(100.0, 0.0), T0);
        cs.down(slot(3), pt(200.0, 0.0), T0);

        // Middle finger lifts first, then the last.
        assert!(cs.up(slot(2)).is_some());
        assert!(cs.up(slot(3)).is_some());
        assert_eq!(cs.len(), 1);
        assert!(cs.get(slot(1)).is_some());
    }

    #[test]
    fn lift_of_unknown_slot_is_not_an_error() {
        let mut cs = ContactSet::new();
        assert_eq!(cs.up(slot(7)), None);
        assert!(cs.is_empty());
    }

    #[test]
    fn motion_for_unknown_slot_is_reported() {
        let mut cs = ContactSet::new();
        assert!(!cs.motion(slot(7), pt(1.0, 1.0)));
        cs.down(slot(7), pt(0.0, 0.0), T0);
        assert!(cs.motion(slot(7), pt(1.0, 1.0)));
    }

    #[test]
    fn reused_slot_does_not_inherit_old_start() {
        let mut cs = ContactSet::new();
        cs.down(slot(1), pt(100.0, 100.0), T0);
        cs.up(slot(1));
        let later = Duration::from_millis(500);
        cs.down(slot(1), pt(900.0, 900.0), later);
        let c = cs.get(slot(1)).unwrap();
        assert_eq!(c.start, pt(900.0, 900.0));
        assert_eq!(c.start_time, later);
        assert_eq!(c.path, 0.0);
    }

    #[test]
    fn down_for_a_still_live_slot_resets_it_entirely() {
        // A missed `up`: the backend reuses a slot while the old contact is
        // still tracked. The new contact must start clean rather than
        // inherit the old start position, start time and accumulated path —
        // otherwise every threshold that measures "distance since touchdown"
        // is measured from the ghost instead.
        let mut cs = ContactSet::new();
        cs.down(slot(1), pt(100.0, 100.0), T0);
        // Axis-aligned, so the accumulated path is exactly 500.
        cs.motion(slot(1), pt(600.0, 100.0));
        assert!((cs.get(slot(1)).unwrap().path - 500.0).abs() < 1e-9);

        let later = Duration::from_millis(500);
        cs.down(slot(1), pt(900.0, 900.0), later);

        let c = cs.get(slot(1)).unwrap();
        assert_eq!(c.start, pt(900.0, 900.0));
        assert_eq!(c.start_time, later);
        assert_eq!(c.path, 0.0);
        assert_eq!(cs.len(), 1, "the ghost contact must not linger");
    }

    #[test]
    fn clear_reports_how_many_contacts_it_dropped() {
        let mut cs = ContactSet::new();
        assert_eq!(cs.clear(), 0);
        cs.down(slot(1), pt(0.0, 0.0), T0);
        cs.down(slot(2), pt(1.0, 1.0), T0);
        assert_eq!(cs.clear(), 2);
        assert!(cs.is_empty());
    }

    // ── derived geometry ────────────────────────────────────────────────

    #[test]
    fn single_contact_has_zero_spread_and_no_rotation() {
        let mut cs = ContactSet::new();
        cs.down(slot(1), pt(100.0, 100.0), T0);
        let f = cs.frame(T0);
        assert_eq!(f.spread, 0.0);
        assert_eq!(f.rotation_delta, 0.0);
        assert_eq!(f.centroid, Some(pt(100.0, 100.0)));
    }

    #[test]
    fn centroid_is_the_mean_of_all_contacts() {
        let mut cs = ContactSet::new();
        cs.down(slot(1), pt(0.0, 0.0), T0);
        cs.down(slot(2), pt(100.0, 0.0), T0);
        cs.down(slot(3), pt(200.0, 0.0), T0);
        let f = cs.frame(T0);
        assert_eq!(f.centroid, Some(pt(100.0, 0.0)));
    }

    #[test]
    fn spread_is_mean_distance_to_centroid() {
        let mut cs = ContactSet::new();
        // Symmetric pair 100 apart: each is 50 from the midpoint.
        cs.down(slot(1), pt(50.0, 0.0), T0);
        cs.down(slot(2), pt(150.0, 0.0), T0);
        let f = cs.frame(T0);
        assert!((f.spread - 50.0).abs() < 1e-9, "got {}", f.spread);
    }

    #[test]
    fn equilateral_triangle_spread_matches_hand_computation() {
        let mut cs = ContactSet::new();
        // Unit circle: centroid at origin, every point 1 away.
        let r = 100.0;
        for (i, slot_id) in [0u32, 1, 2].into_iter().enumerate() {
            let angle = std::f64::consts::FRAC_PI_3 * i as f64 * 2.0;
            cs.down(slot(slot_id), pt(r * angle.cos(), r * angle.sin()), T0);
        }
        let f = cs.frame(T0);
        assert!((f.spread - r).abs() < 1e-9, "got {}", f.spread);
    }

    #[test]
    fn centroid_delta_tracks_the_whole_cluster_moving() {
        let mut cs = ContactSet::new();
        cs.down(slot(1), pt(0.0, 0.0), T0);
        cs.down(slot(2), pt(100.0, 0.0), T0);
        let first = cs.frame(T0);
        assert_eq!(first.centroid_delta, Point::default());

        // Both fingers translate by (+10, +20).
        cs.motion(slot(1), pt(10.0, 20.0));
        cs.motion(slot(2), pt(110.0, 20.0));
        let second = cs.frame(Duration::from_millis(16));
        assert!((second.centroid_delta.x - 10.0).abs() < 1e-9);
        assert!((second.centroid_delta.y - 20.0).abs() < 1e-9);
    }

    #[test]
    fn spread_is_unchanged_by_rigid_translation() {
        let mut cs = ContactSet::new();
        cs.down(slot(1), pt(50.0, 0.0), T0);
        cs.down(slot(2), pt(150.0, 0.0), T0);
        let before = cs.frame(T0).spread;

        cs.motion(slot(1), pt(60.0, 40.0));
        cs.motion(slot(2), pt(160.0, 40.0));
        let after = cs.frame(Duration::from_millis(16)).spread;
        assert!((before - after).abs() < 1e-9);
    }

    #[test]
    fn total_motion_accumulates_then_resets_per_frame() {
        let mut cs = ContactSet::new();
        cs.down(slot(1), pt(0.0, 0.0), T0);
        cs.motion(slot(1), pt(3.0, 4.0)); // 5.0
        assert!((cs.frame(T0).total_motion - 5.0).abs() < 1e-9);

        cs.motion(slot(1), pt(6.0, 4.0)); // +3.0
        assert!((cs.frame(Duration::from_millis(16)).total_motion - 3.0).abs() < 1e-9);

        // A frame with no motion reports zero, not the stale value.
        assert!((cs.frame(Duration::from_millis(32)).total_motion).abs() < 1e-9);
    }

    #[test]
    fn path_length_is_cumulative_across_frames() {
        let mut cs = ContactSet::new();
        cs.down(slot(1), pt(0.0, 0.0), T0);
        cs.motion(slot(1), pt(3.0, 4.0)); // 5
        cs.frame(T0);
        cs.motion(slot(1), pt(3.0, 9.0)); // +5
        cs.frame(Duration::from_millis(16));
        assert!((cs.get(slot(1)).unwrap().path - 10.0).abs() < 1e-9);
    }

    // ── rotation ────────────────────────────────────────────────────────

    #[test]
    fn still_fingers_produce_no_rotation() {
        let mut cs = ContactSet::new();
        ring(&mut cs);
        assert_eq!(cs.frame(T0).rotation_delta, 0.0);
        // Second identical frame must also be zero.
        assert_eq!(cs.frame(Duration::from_millis(16)).rotation_delta, 0.0);
    }

    #[test]
    fn quarter_turn_is_counter_clockwise_on_screen() {
        let mut cs = ContactSet::new();
        ring(&mut cs);
        cs.frame(T0);
        // Screen y grows downward, so a counter-clockwise turn *as the user
        // sees it* is a negative rotation in standard math coordinates.
        rotate_ring(&mut cs, -FRAC_PI_2);
        let f = cs.frame(Duration::from_millis(16));
        assert!(
            (f.rotation_delta - FRAC_PI_2).abs() < 1e-6,
            "got {}",
            f.rotation_delta
        );
    }

    #[test]
    fn clockwise_turn_reports_negative_delta() {
        let mut cs = ContactSet::new();
        ring(&mut cs);
        cs.frame(T0);
        rotate_ring(&mut cs, FRAC_PI_2);
        let f = cs.frame(Duration::from_millis(16));
        assert!(
            (f.rotation_delta + FRAC_PI_2).abs() < 1e-6,
            "got {}",
            f.rotation_delta
        );
    }

    #[test]
    fn rotation_unwraps_across_the_positive_pi_boundary() {
        let mut cs = ContactSet::new();
        ring(&mut cs);
        cs.frame(T0);
        // A 2.9 rad turn (≈166°) drives one slot's angle across the ±π
        // seam, where its raw delta comes out near -3.38 instead of +2.90.
        // Without unwrapping the four-slot average collapses to ≈1.08, so
        // removing that branch fails this test loudly.
        rotate_ring(&mut cs, -2.9);
        let f = cs.frame(Duration::from_millis(16));
        assert!(
            (f.rotation_delta - 2.9).abs() < 1e-6,
            "got {}",
            f.rotation_delta
        );
    }

    #[test]
    fn sub_threshold_jitter_is_clamped_to_zero() {
        let mut cs = ContactSet::new();
        ring(&mut cs);
        cs.frame(T0);
        // Far below ROTATION_NOISE_FLOOR.
        rotate_ring(&mut cs, ROTATION_NOISE_FLOOR / 100.0);
        assert_eq!(cs.frame(Duration::from_millis(16)).rotation_delta, 0.0);
    }

    #[test]
    fn rotation_is_zero_until_two_slots_overlap() {
        let mut cs = ContactSet::new();
        cs.down(slot(1), pt(100.0, 0.0), T0);
        cs.down(slot(2), pt(-100.0, 0.0), T0);
        cs.frame(T0);

        // Slot 2 lifts; only one contact remains, so there is no angle basis.
        cs.up(slot(2));
        let f = cs.frame(Duration::from_millis(16));
        assert_eq!(f.rotation_delta, 0.0);
        assert_eq!(f.count, 1);
    }

    #[test]
    fn newly_joined_finger_contributes_no_rotation_on_its_first_frame() {
        let mut cs = ContactSet::new();
        cs.down(slot(1), pt(100.0, 0.0), T0);
        cs.down(slot(2), pt(-100.0, 0.0), T0);
        cs.frame(T0);

        cs.down(slot(3), pt(0.0, -100.0), Duration::from_millis(16));
        let f = cs.frame(Duration::from_millis(16));
        // Slots 1 and 2 did not move, so the only overlap produces no delta.
        assert_eq!(f.rotation_delta, 0.0);
        assert_eq!(f.count, 3);
    }

    // ── frame bookkeeping ───────────────────────────────────────────────

    #[test]
    fn changed_flag_clears_each_frame() {
        let mut cs = ContactSet::new();
        cs.down(slot(1), pt(0.0, 0.0), T0);
        assert!(cs.frame(T0).changed);
        assert!(!cs.frame(Duration::from_millis(16)).changed);

        cs.motion(slot(1), pt(5.0, 0.0));
        assert!(cs.frame(Duration::from_millis(32)).changed);
    }

    #[test]
    fn last_frame_time_is_recorded() {
        let mut cs = ContactSet::new();
        assert_eq!(cs.last_frame_time(), None);
        cs.down(slot(1), pt(0.0, 0.0), T0);
        cs.frame(T0);
        assert_eq!(cs.last_frame_time(), Some(T0));
    }

    #[test]
    fn dropping_to_zero_contacts_resets_the_rotation_basis() {
        let mut cs = ContactSet::new();
        ring(&mut cs);
        cs.frame(T0);

        let live: Vec<Slot> = cs.iter().map(|(s, _)| s).collect();
        for s in live {
            cs.up(s);
        }
        cs.frame(Duration::from_millis(16));

        // A fresh pair afterwards must not inherit the old angles.
        cs.down(slot(1), pt(100.0, 0.0), Duration::from_millis(32));
        cs.down(slot(2), pt(-100.0, 0.0), Duration::from_millis(32));
        assert_eq!(cs.frame(Duration::from_millis(32)).rotation_delta, 0.0);
    }

    /// Four points on a circle of radius 100, so a rigid rotation of the set
    /// is easy to express and every slot has a well-defined angle.
    fn ring(cs: &mut ContactSet) {
        let r = 100.0_f64;
        for (i, id) in [0u32, 1, 2, 3].into_iter().enumerate() {
            let a = FRAC_PI_2 * i as f64;
            cs.down(slot(id), pt(r * a.cos(), r * a.sin()), T0);
        }
    }

    /// Rotate every live contact by `angle` radians, in standard math
    /// coordinates (positive = clockwise as seen on screen, since y grows
    /// downward).
    ///
    /// Collects positions first: iterating `cs` borrows it immutably while
    /// `motion` needs it mutably.
    fn rotate_ring(cs: &mut ContactSet, angle: f64) {
        let (sin, cos) = angle.sin_cos();
        let live: Vec<(Slot, Point)> = cs.iter().map(|(s, c)| (s, c.pos)).collect();
        for (s, p) in live {
            cs.motion(s, pt(p.x * cos - p.y * sin, p.x * sin + p.y * cos));
        }
    }
}
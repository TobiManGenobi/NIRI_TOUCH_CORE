//! `touch-core` — layers L1 and L2 of the niri-toublet touch stack.
//!
//! See `DESIGN.md` at the repository root for the full architecture. In
//! short, the touch stack is three layers with one-way dependencies:
//!
//! ```text
//! ┌─ L3  ARBITRATION ──────────────────────────────────┐
//! │  Who owns this touch? window rules, exclusion rects  │
//! ├─ L2  RECOGNITION ──────────────────────────────────┤
//! │  Pure state machines. ContactFrame* -> Gesture*     │
//! ├─ L1  CONTACT ─────────────────────────────────────┐
//! │  Slots, centroid, spread, angle, zone              │
//! └─────────────────────────────────────────────────────┘
//! ```
//!
//! This crate is **L1 and L2 only**. Both are pure: no compositor, no
//! Wayland, no clock, no configuration, no actions. That is the property
//! that makes the gesture stack testable in milliseconds and reviewable in
//! isolation.
//!
//! # Why the split matters
//!
//! Both prior forks fused all three layers into niri's touch-down hot path.
//! niri-tablet then needed four of its twenty patches purely to repair the
//! ownership logic that fell out of that fusion (late-joining fingers,
//! sequence ownership, the smithay grab seam, device-removal state). Those
//! bugs are all *bookkeeping against a live seat*, which is exactly the part
//! that cannot be tested without one.
//!
//! Lifting contact tracking and recognition out means the class of bug that
//! produced patches `0010`/`0011`/`0012` has nowhere to live: it is confined
//! to the thin L3 adapter, where it can be tested against fakes.
//!
//! # Determinism
//!
//! Time is passed in as a [`Duration`] rather than read from a clock. Golden
//! traces must be byte-reproducible across machines and across runs, so no
//! layer below L3 is allowed to call [`std::time::Instant::now`].
//!
//! # Current state
//!
//! - [`contact`] — L1, contact tracking and derived geometry. Done.
//! - [`gesture`] — L2, the recogniser. Done.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod contact;
pub mod gesture;

pub use contact::{
    classify_zone, Contact, ContactSet, Corner, Edge, Frame, OutputSize, Point, Slot, Third, Zone,
    DEFAULT_EDGE_ZONE_PX, ROTATION_NOISE_FLOOR,
};
pub use gesture::{
    Gesture, GestureEvent, PinchDirection, Recognizer, RecognizerConfig, RotateDirection,
    SwipeDirection,
};

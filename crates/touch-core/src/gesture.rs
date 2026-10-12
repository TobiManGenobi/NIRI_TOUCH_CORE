//! L2 — gesture recognition.
//!
//! Consumes the per-frame geometry from L1 ([`crate::contact::Frame`]) and
//! emits a stream of [`GestureEvent`]s. Pure: no compositor, no Wayland, no
//! clock, no configuration parser, no actions.
//!
//! # Lifecycle
//!
//! A recognised gesture has a strict lifecycle:
//!
//! ```text
//! Begin ──▶ Update* ──▶ End { completed }
//! ```
//!
//! Exactly one `Begin`, exactly one `End`. [`Recognizer::end_active`] is the
//! only path that clears an active gesture, and every exit — finger lift,
//! device removal, touch cancel — routes through it. A `Begin` can never go
//! unanswered. That invariant is why this layer is separate from L3:
//! niri-tablet patch `0011` is a postmortem of exactly this going wrong
//! against a live seat.
//!
//! `End` carries `completed`, meaning the gesture reached `progress: 1.0`.
//! L3 fires an action only on `End { completed: true }`, so a swipe the user
//! abandoned below threshold is silently discarded.
//!
//! # Arbitration
//!
//! Three signals can cross threshold in the same frame: centroid travel
//! (swipe), spread change (pinch), accumulated rotation. They are arbitrated
//! by **relative overshoot** — the signal furthest past its own threshold:
//!
//! ```text
//! ratio = metric / threshold        recognise argmax(ratio), if argmax >= 1.0
//! ```
//!
//! No priority table needed, and retuning a threshold changes only how
//! *eagerly* a gesture wins, not what it competes against.
//!
//! # Direction and axis lock
//!
//! Once recognised, a gesture's kind is fixed until it ends — a swipe that
//! drifts into a pinch stays a swipe. A swipe's axis locks at recognition, so
//! a diagonal drag reports a stable direction instead of flickering between
//! `left` and `up` as noise changes the dominant component.
//!
//! # Re-baselining
//!
//! Metrics are measured from a baseline captured at the last *finger-count
//! change*, not at first touchdown. A finger landing late therefore measures
//! travel from where the cluster actually is, not from where the first finger
//! happened to land — the bug niri-tablet patch `0006` exists to correct.

use std::time::Duration;

use crate::contact::{Contact, ContactSet, Corner, Edge, Frame, Point, Slot, Third, Zone};

/// Direction of a translational gesture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SwipeDirection {
    /// Toward the top of the screen.
    Up,
    /// Toward the bottom of the screen.
    Down,
    /// Toward the left of the screen.
    Left,
    /// Toward the right of the screen.
    Right,
}

/// Direction of a radial gesture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PinchDirection {
    /// Fingers converging.
    In,
    /// Fingers diverging.
    Out,
}

/// Direction of a rotational gesture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RotateDirection {
    /// Counter-clockwise as the user sees it.
    CounterClockwise,
    /// Clockwise as the user sees it.
    Clockwise,
}

/// A recognised gesture.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Gesture {
    /// Multi-finger translation.
    Swipe {
        /// Contacts down when it locked.
        fingers: u8,
        /// Locked direction.
        direction: SwipeDirection,
        /// Displacement along the locked axis, in pixels. Non-negative;
        /// `direction` carries the sense. Keeping it a magnitude avoids the
        /// trap where `Up` — which moves toward negative y — reports a
        /// negative delta while still agreeing with its own direction.
        delta: f64,
        /// Normalised completion in `0.0..=1.0`.
        progress: f64,
    },
    /// Radial convergence or divergence.
    Pinch {
        /// Contacts down when it locked.
        fingers: u8,
        /// Locked direction.
        direction: PinchDirection,
        /// Signed spread change, in pixels. Positive is divergence.
        delta: f64,
        /// Normalised completion in `0.0..=1.0`.
        progress: f64,
    },
    /// Rotation about the contact centroid.
    Rotate {
        /// Contacts down when it locked.
        fingers: u8,
        /// Locked direction.
        direction: RotateDirection,
        /// Magnitude of rotation, in radians. Always non-negative.
        delta: f64,
        /// Normalised completion in `0.0..=1.0`.
        progress: f64,
    },
    /// All contacts lifted with negligible motion. Fires on lift.
    Tap {
        /// Contacts that participated.
        fingers: u8,
    },
    /// A tap candidate started moving. Fires at the instant the wobble budget
    /// is exceeded — before the follow-on gesture is recognised — so L3 can
    /// drive a continuous action beginning before the finger lifts.
    TapHold {
        /// Contacts that participated.
        fingers: u8,
        /// Direction at the moment motion was detected, if determinable.
        direction: Option<SwipeDirection>,
    },
    /// Single contact dragging inward from a screen edge.
    EdgeSwipe {
        /// Which edge it started from.
        edge: Edge,
        /// Which third of that edge.
        third: Third,
        /// Displacement along the locked axis, in pixels. Non-negative.
        delta: f64,
        /// Normalised completion in `0.0..=1.0`.
        progress: f64,
    },
    /// Single contact dragging diagonally inward from a corner.
    CornerSwipe {
        /// Which corner it started from.
        corner: Corner,
        /// Displacement along the locked axis, in pixels. Non-negative.
        delta: f64,
        /// Normalised completion in `0.0..=1.0`.
        progress: f64,
    },
}

impl Gesture {
    /// Contacts that drove this gesture.
    #[must_use]
    pub const fn fingers(&self) -> u8 {
        match *self {
            Gesture::Swipe { fingers, .. }
            | Gesture::Pinch { fingers, .. }
            | Gesture::Rotate { fingers, .. }
            | Gesture::Tap { fingers }
            | Gesture::TapHold { fingers, .. } => fingers,
            Gesture::EdgeSwipe { .. } | Gesture::CornerSwipe { .. } => 1,
        }
    }

    /// Normalised completion, for continuous gestures.
    #[must_use]
    pub const fn progress(&self) -> Option<f64> {
        match *self {
            Gesture::Swipe { progress, .. }
            | Gesture::Pinch { progress, .. }
            | Gesture::Rotate { progress, .. }
            | Gesture::EdgeSwipe { progress, .. }
            | Gesture::CornerSwipe { progress, .. } => Some(progress),
            Gesture::Tap { .. } | Gesture::TapHold { .. } => None,
        }
    }

    /// Whether this gesture emits `Update`s.
    #[must_use]
    pub const fn is_continuous(&self) -> bool {
        self.progress().is_some()
    }

    /// Whether the gesture reached its action threshold.
    #[must_use]
    pub fn is_completed(&self) -> bool {
        self.progress().is_some_and(|p| p >= 1.0)
    }
}

/// A lifecycle event for a recognised gesture.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GestureEvent {
    /// A gesture was recognised. Carries its initial state.
    Begin(Gesture),
    /// A continuous gesture advanced.
    Update(Gesture),
    /// A gesture finished.
    End {
        /// Final gesture state.
        gesture: Gesture,
        /// Whether the action threshold was reached.
        completed: bool,
    },
}

/// Tuning for the recogniser.
///
/// Defaults are conservative for touch hardware, where contact noise is much
/// higher than on a touchpad.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RecognizerConfig {
    /// Fewest contacts that can drive a multi-finger gesture.
    pub min_fingers: u8,
    /// Most contacts tracked at once.
    pub max_fingers: u8,
    /// Centroid travel in pixels before a swipe is recognised.
    pub swipe_threshold: f64,
    /// Spread change in pixels before a pinch is recognised.
    pub pinch_threshold: f64,
    /// Accumulated rotation in radians before a rotation is recognised.
    pub rotate_threshold: f64,
    /// How far a contact may drift from its touchdown, in pixels, before the
    /// sequence stops being a tap candidate.
    pub tap_wobble_threshold: f64,
    /// How long contacts may rest before the sequence stops being a tap
    /// candidate.
    pub tap_max_duration: Duration,
    /// Centroid travel in pixels before an edge or corner swipe is
    /// recognised.
    pub edge_threshold: f64,
    /// Displacement at which a translational gesture reports `progress: 1.0`.
    pub progress_distance: f64,
    /// Rotation at which a rotate gesture reports `progress: 1.0`.
    pub progress_radians: f64,
}

impl Default for RecognizerConfig {
    fn default() -> Self {
        Self {
            min_fingers: 2,
            max_fingers: 10,
            swipe_threshold: 30.0,
            pinch_threshold: 30.0,
            rotate_threshold: 0.15,
            tap_wobble_threshold: 24.0,
            tap_max_duration: Duration::from_millis(500),
            edge_threshold: 30.0,
            progress_distance: 300.0,
            progress_radians: 0.8,
        }
    }
}

/// The dominant axis of a displacement vector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Axis {
    Horizontal,
    Vertical,
}

impl Axis {
    fn of(v: Point) -> Axis {
        if v.x.abs() >= v.y.abs() {
            Axis::Horizontal
        } else {
            Axis::Vertical
        }
    }

    fn project(self, v: Point) -> f64 {
        match self {
            Axis::Horizontal => v.x,
            Axis::Vertical => v.y,
        }
    }
}

fn swipe_direction(v: Point) -> SwipeDirection {
    if v.x.abs() >= v.y.abs() {
        if v.x < 0.0 {
            SwipeDirection::Left
        } else {
            SwipeDirection::Right
        }
    } else if v.y < 0.0 {
        SwipeDirection::Up
    } else {
        SwipeDirection::Down
    }
}

/// The metrics a candidate gesture is measured against.
#[derive(Debug, Clone)]
struct Baseline {
    centroid: Point,
    spread: f64,
    /// Signed accumulated rotation since the baseline.
    rotation: f64,
}

#[derive(Debug, Clone)]

enum Active {
    Swipe {
        fingers: u8,
        direction: SwipeDirection,
        axis: Axis,
        origin: Point,
    },
    Pinch {
        fingers: u8,
        direction: PinchDirection,
        origin_spread: f64,
    },
    Rotate {
        fingers: u8,
        direction: RotateDirection,
    },
    EdgeSwipe {
        edge: Edge,
        third: Third,
        axis: Axis,
        origin: Point,
    },
    CornerSwipe {
        corner: Corner,
        axis: Axis,
        origin: Point,
    },
}

/// The gesture recogniser.
///
/// Owns an L1 [`ContactSet`], so callers feed raw contact events and read back
/// gestures.
#[derive(Debug, Clone)]
pub struct Recognizer {
    config: RecognizerConfig,
    set: ContactSet,
    active: Option<Active>,
    baseline: Option<Baseline>,
    /// Boundary classification of the sequence's *first* contact. Only set
    /// when a single contact starts the sequence, since edge and corner
    /// gestures are single-finger by definition.
    pending_zone: Option<Zone>,
    /// When the sequence's first contact landed.
    started: Option<Duration>,
    /// Whether the sequence is still eligible to be a tap.
    tap_viable: bool,
    /// Largest contact count seen this sequence.
    peak_fingers: u8,
    /// Contact count at the previous sealed frame, used to detect changes.
    prev_count: usize,
    /// Last centroid seen while contacts were live, so `End` can still name a
    /// gesture after the set has emptied.
    last_centroid: Option<Point>,
    /// Last spread seen while contacts were live, likewise.
    last_spread: f64,
}

impl Default for Recognizer {
    fn default() -> Self {
        Self::new(RecognizerConfig::default())
    }
}

impl Recognizer {
    /// A recogniser with the given tuning.
    #[must_use]
    pub fn new(config: RecognizerConfig) -> Self {
        Self {
            config,
            set: ContactSet::new(),
            active: None,
            baseline: None,
            pending_zone: None,
            started: None,
            tap_viable: false,
            peak_fingers: 0,
            prev_count: 0,
            last_centroid: None,
            last_spread: 0.0,
        }
    }

    /// Current tuning.
    #[must_use]
    pub fn config(&self) -> &RecognizerConfig {
        &self.config
    }

    /// Whether a gesture is currently recognised.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.active.is_some()
    }

    /// Live contacts, for touch-point visualisation and policy decisions.
    pub fn contacts(&self) -> impl Iterator<Item = (Slot, &Contact)> {
        self.set.iter()
    }

    /// Record a contact landing.
    ///
    /// `zone` is the boundary classification L3 computed for this contact.
    /// It is consulted only when it starts a sequence.
    pub fn down(&mut self, slot: Slot, pos: Point, zone: Option<Zone>, time: Duration) {
        let starts_sequence = self.set.is_empty();
        self.set.down(slot, pos, time);
        self.peak_fingers = self.peak_fingers.max(self.set.len() as u8);

        if starts_sequence {
            self.started = Some(time);
            self.pending_zone = zone;
            self.tap_viable = true;
        }
    }

    /// Record a contact moving. Returns `false` if the slot was untracked.
    pub fn motion(&mut self, slot: Slot, pos: Point) -> bool {
        self.set.motion(slot, pos)
    }

    /// Record a contact lifting.
    pub fn up(&mut self, slot: Slot) {
        self.set.up(slot);
    }

    /// Seal a batch and return the resulting gesture events.
    pub fn frame(&mut self, time: Duration) -> Vec<GestureEvent> {
        let mut out = Vec::new();
        let frame = self.set.frame(time);

        if let Some(centroid) = frame.centroid {
            self.last_centroid = Some(centroid);
            self.last_spread = frame.spread;
        }

        // An active gesture owns the sequence until it ends.
        if self.active.is_some() {
            let ended = frame.count == 0;
            match self.active_gesture() {
                Some(gesture) if ended => {
                    out.push(GestureEvent::End {
                        completed: gesture.is_completed(),
                        gesture,
                    });
                    self.reset_sequence();
                }
                Some(gesture) => out.push(GestureEvent::Update(gesture)),
                None => self.end_active(&mut out),
            }
            self.prev_count = frame.count;
            return out;
        }

        // Nothing recognised: the sequence may still be a tap.
        if frame.count == 0 {
            if self.tap_viable && self.peak_fingers > 0 {
                let gesture = Gesture::Tap {
                    fingers: self.peak_fingers,
                };
                out.push(GestureEvent::Begin(gesture));
                out.push(GestureEvent::End {
                    gesture,
                    completed: true,
                });
            }
            self.reset_sequence();
            return out;
        }

        if self.tap_viable {
            let elapsed = self
                .started
                .map_or(Duration::ZERO, |s| time.saturating_sub(s));
            if elapsed > self.config.tap_max_duration {
                self.tap_viable = false;
            }
            if self.wobble() > self.config.tap_wobble_threshold {
                self.tap_viable = false;
                let direction = match (self.baseline.as_ref(), frame.centroid) {
                    (Some(b), Some(c)) => Some(swipe_direction(c - b.centroid)),
                    _ => None,
                };
                let gesture = Gesture::TapHold {
                    fingers: self.peak_fingers,
                    direction,
                };
                out.push(GestureEvent::Begin(gesture));
                out.push(GestureEvent::End {
                    gesture,
                    completed: true,
                });
            }
        }

        // Re-baseline whenever the contact count changes, so travel is
        // measured from where the cluster is now.
        if frame.count != self.prev_count {
            self.rebaseline();
        } else if let Some(b) = self.baseline.as_mut() {
            b.rotation += frame.rotation_delta;
        }
        self.prev_count = frame.count;

        if let Some(active) = self.try_recognise(&frame) {
            if let Some(gesture) = self.begin(active) {
                out.push(GestureEvent::Begin(gesture));
            }
        }

        out
    }

    /// Abandon the sequence — device removal, touch cancel, config reload.
    ///
    /// Emits `End` for an active gesture, so a `Begin` can never go
    /// unanswered. Never reports `completed`: the gesture was cut short.
    pub fn reset(&mut self) -> Vec<GestureEvent> {
        let mut out = Vec::new();
        self.end_active(&mut out);
        self.set.clear();
        self.reset_sequence();
        out
    }

    // ── internals ───────────────────────────────────────────────────────

    /// Snapshot an active gesture's current state.
    ///
    /// Pure read: all inputs are captured before any `&mut self` field access,
    /// which keeps the state machine free of interleaved mutation.
    fn active_gesture(&self) -> Option<Gesture> {
        let active = self.active.as_ref()?;
        let centroid = self.last_centroid?;
        let spread = self.last_spread;
        let cfg = self.config;
        let linear = |d: f64| (d.abs() / cfg.progress_distance).clamp(0.0, 1.0);

        Some(match active {
            Active::Swipe {
                fingers,
                direction,
                axis,
                origin,
                ..
            } => {
                let v = centroid - *origin;
                let d = axis.project(v).abs();
                Gesture::Swipe {
                    fingers: *fingers,
                    direction: *direction,
                    delta: d,
                    progress: linear(d),
                }
            }
            Active::Pinch {
                fingers,
                direction,
                origin_spread,
            } => {
                let d = spread - *origin_spread;
                Gesture::Pinch {
                    fingers: *fingers,
                    direction: *direction,
                    delta: d,
                    progress: linear(d),
                }
            }
            Active::Rotate { fingers, direction } => Gesture::Rotate {
                fingers: *fingers,
                direction: *direction,
                delta: self.rotation_accum(),
                progress: (self.rotation_accum() / cfg.progress_radians).clamp(0.0, 1.0),
            },
            Active::EdgeSwipe {
                edge,
                third,
                axis,
                origin,
                ..
            } => {
                let v = centroid - *origin;
                let d = axis.project(v).abs();
                Gesture::EdgeSwipe {
                    edge: *edge,
                    third: *third,
                    delta: d,
                    progress: linear(d),
                }
            }
            Active::CornerSwipe {
                corner,
                axis,
                origin,
                ..
            } => {
                let v = centroid - *origin;
                let d = axis.project(v).abs();
                Gesture::CornerSwipe {
                    corner: *corner,
                    delta: d,
                    progress: linear(d),
                }
            }
        })
    }

    /// The only place a gesture is installed.
    fn begin(&mut self, active: Active) -> Option<Gesture> {
        self.active = Some(active);
        self.active_gesture()
    }

    /// The only place an active gesture is cleared.
    fn end_active(&mut self, out: &mut Vec<GestureEvent>) {
        if let Some(gesture) = self.active_gesture() {
            out.push(GestureEvent::End {
                gesture,
                completed: false,
            });
        }
        self.active = None;
    }

    /// Accumulated rotation since the active gesture was recognised.
    fn rotation_accum(&self) -> f64 {
        self.baseline.as_ref().map_or(0.0, |b| b.rotation).abs()
    }

    fn rebaseline(&mut self) {
        let Some(centroid) = self.set.centroid_opt() else {
            self.baseline = None;
            return;
        };
        self.baseline = Some(Baseline {
            spread: self.set.spread(centroid),
            centroid,
            rotation: 0.0,
        });
    }

    fn reset_sequence(&mut self) {
        self.active = None;
        self.baseline = None;
        self.pending_zone = None;
        self.started = None;
        self.tap_viable = false;
        self.peak_fingers = 0;
        self.prev_count = 0;
        self.last_centroid = None;
        self.last_spread = 0.0;
    }

    /// Largest distance any contact has drifted from its own touchdown.
    fn wobble(&self) -> f64 {
        self.set
            .iter()
            .map(|(_, c)| c.pos.distance_to(c.start))
            .fold(0.0_f64, f64::max)
    }

    fn try_recognise(&self, frame: &Frame) -> Option<Active> {
        let centroid = frame.centroid?;
        let base = self.baseline.as_ref()?;
        let cfg = self.config;
        let count = self.set.len() as u8;

        // Single-finger drags inside an edge or corner zone are that gesture
        // and never compete with the multi-finger paths.
        if count == 1 {
            if let Some(zone) = self.pending_zone {
                if centroid.distance_to(base.centroid) >= cfg.edge_threshold {
                    let v = centroid - base.centroid;
                    let axis = Axis::of(v);
                    return Some(match zone {
                        Zone::Edge { edge, third } => Active::EdgeSwipe {
                            edge,
                            third,
                            axis,
                            origin: base.centroid,
                        },
                        Zone::Corner { corner } => Active::CornerSwipe {
                            corner,
                            axis,
                            origin: base.centroid,
                        },
                    });
                }
            }
            if usize::from(count) < usize::from(cfg.min_fingers) {
                return None;
            }
        }

        if count < cfg.min_fingers || count > cfg.max_fingers {
            return None;
        }

        let travel = centroid.distance_to(base.centroid);
        let spread_delta = frame.spread - base.spread;
        let rotation = base.rotation;

        // Rotation is only admissible when the cluster is *not* travelling.
        //
        // A rigid rotation holds the centroid still while the spread stays
        // wide, so `travel < spread` is the natural admissibility test. The
        // gate is not cosmetic: for collinear contacts — the ordinary shape
        // of a vertical multi-finger swipe, fingers stacked in a line — the
        // per-slot angles are ill-conditioned, and a plain downward drag
        // produces a phantom rotation of ~π/6. That is 3.5× its threshold,
        // enough to beat a real swipe on relative overshoot.
        let rotation_ratio = if travel < base.spread {
            rotation.abs() / cfg.rotate_threshold
        } else {
            0.0
        };

        let candidates = [
            (travel / cfg.swipe_threshold, 0u8),
            (spread_delta.abs() / cfg.pinch_threshold, 1u8),
            (rotation_ratio, 2u8),
        ];
        let &(best_ratio, best_kind) = candidates.iter().max_by(|a, b| a.0.total_cmp(&b.0))?;
        if best_ratio < 1.0 {
            return None;
        }

        let v = centroid - base.centroid;
        Some(match best_kind {
            1 => Active::Pinch {
                fingers: count,
                direction: if spread_delta < 0.0 {
                    PinchDirection::In
                } else {
                    PinchDirection::Out
                },
                origin_spread: base.spread,
            },
            2 => Active::Rotate {
                fingers: count,
                direction: if rotation < 0.0 {
                    RotateDirection::Clockwise
                } else {
                    RotateDirection::CounterClockwise
                },
            },
            _ => Active::Swipe {
                fingers: count,
                direction: swipe_direction(v),
                axis: Axis::of(v),
                origin: base.centroid,
            },
        })
    }
}

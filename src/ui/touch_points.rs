//! Touch point visualization: translucent dots that follow the fingers.
//!
//! Fed from the touch handlers as a pure observer — it never consumes events
//! and does not interfere with gesture recognition or client delivery. The
//! compositor renders the dots on top of everything, so any screen recording
//! captures them. Meant for demo videos and for debugging gestures; enabled
//! with `gestures { show-touch-points all; }`.
//!
//! # Why the handlers feed this rather than the render pass
//!
//! The obvious alternative is to read the live-contact ledger during the
//! render pass. That is wrong for this feature for two independent reasons,
//! both fatal rather than merely awkward:
//!
//! * `LiveContact::pos` is where the contact *landed*. Nothing updates it on
//!   motion, so a dragging finger's dot would freeze at the touchdown point —
//!   and following the finger is the entire point of the overlay.
//! * A lift *deletes* the ledger entry. A render pass therefore cannot tell
//!   "this finger just left, fade it out at where it was" from "this finger
//!   was never here", so fade-out would need a shadow copy of the state
//!   anyway. Driving from the handlers gets motion and lift for free.

use std::cell::RefCell;
use std::collections::HashMap;

use niri_config::gestures::ShowTouchPoints;
use ordered_float::NotNan;
use pangocairo::cairo::{self, ImageSurface};
use smithay::backend::input::TouchSlot;
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::output::Output;
use smithay::reexports::gbm::Format as Fourcc;
use smithay::utils::{Logical, Point, Rectangle, Transform};

use crate::animation::{Animation, Clock};
use crate::render_helpers::primary_gpu_texture::PrimaryGpuTextureRenderElement;
use crate::render_helpers::renderer::NiriRenderer;
use crate::render_helpers::texture::{TextureBuffer, TextureRenderElement};
use crate::utils::to_physical_precise_round;

/// Logical diameter of a touch point dot.
const DOT_DIAMETER: f64 = 28.0;
/// How long a dot takes to fade in after the finger goes down.
const APPEAR_MS: u32 = 80;
/// How long a dot keeps fading after the finger lifts.
const FADE_OUT_MS: u32 = 250;

pub struct TouchPoints {
    clock: Clock,
    dots: HashMap<TouchSlot, Dot>,
    /// Circle texture, cached per output scale the way the config error
    /// notification caches its text texture.
    buffers: RefCell<HashMap<NotNan<f64>, Option<TextureBuffer<GlesTexture>>>>,
}

enum Dot {
    /// Finger on the screen; the dot is fading in (or fully shown).
    Active {
        pos: Point<f64, Logical>,
        appear: Animation,
        /// Whether this dot has been on screen: enough fingers were down
        /// concurrently at some point during its life. In gestures mode,
        /// never-qualified dots never render — and must not pop in at lift
        /// just to fade out.
        qualified: bool,
    },
    /// Finger lifted; the dot fades out at the last known position.
    Released {
        pos: Point<f64, Logical>,
        fade: Animation,
        qualified: bool,
    },
}

impl TouchPoints {
    pub fn new(clock: Clock) -> Self {
        Self {
            clock,
            dots: HashMap::new(),
            buffers: RefCell::new(HashMap::new()),
        }
    }

    fn animation(clock: &Clock, from: f64, to: f64, duration_ms: u32) -> Animation {
        Animation::new(
            clock.clone(),
            from,
            to,
            0.,
            niri_config::Animation {
                off: false,
                kind: niri_config::animations::Kind::Easing(
                    niri_config::animations::EasingParams {
                        duration_ms,
                        curve: niri_config::animations::Curve::EaseOutQuad,
                    },
                ),
            },
        )
    }

    /// Record a finger going down at a global logical position.
    pub fn down(&mut self, slot: TouchSlot, pos: Point<f64, Logical>, min_fingers: usize) {
        let appear = Self::animation(&self.clock, 0., 1., APPEAR_MS);
        self.dots.insert(
            slot,
            Dot::Active {
                pos,
                appear,
                qualified: false,
            },
        );
        self.update_qualification(min_fingers);
    }

    /// Move a finger. A lifted or unknown slot has nothing to move.
    pub fn motion(&mut self, slot: TouchSlot, pos: Point<f64, Logical>, min_fingers: usize) {
        match self.dots.get_mut(&slot) {
            Some(Dot::Active { pos: p, .. }) => *p = pos,
            Some(Dot::Released { .. }) | None => {}
        }
        self.update_qualification(min_fingers);
    }

    /// Marks all active dots as qualified once enough fingers are down
    /// concurrently — including late joiners that arrived before the
    /// threshold was reached, so a three-finger gesture does not make the
    /// first two fingers' dots pop in only at the third.
    fn update_qualification(&mut self, min_fingers: usize) {
        if self.active_count() >= min_fingers.max(1) {
            for dot in self.dots.values_mut() {
                if let Dot::Active { qualified, .. } = dot {
                    *qualified = true;
                }
            }
        }
    }

    /// A finger lifted. The dot remembers its position, so the positionless
    /// up event only has to start the fade-out.
    pub fn up(&mut self, slot: TouchSlot) {
        if let Some(Dot::Active { pos, qualified, .. }) = self.dots.remove(&slot) {
            let fade = Self::animation(&self.clock, 1., 0., FADE_OUT_MS);
            self.dots.insert(
                slot,
                Dot::Released {
                    pos,
                    fade,
                    qualified,
                },
            );
        }
    }

    /// The backend cancelled the touches (device removed, seat torn down):
    /// fade everything out rather than leaving phantom dots forever.
    pub fn cancel(&mut self) {
        let slots: Vec<_> = self.dots.keys().cloned().collect();
        for slot in slots {
            self.up(slot);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.dots.is_empty()
    }

    /// Dots that are currently on the glass, with their global positions.
    /// Exposed for tests; rendering uses [`Self::renderable_dots`].
    pub fn active_positions(&self) -> Vec<Point<f64, Logical>> {
        self.dots
            .values()
            .filter_map(|dot| match dot {
                Dot::Active { pos, .. } => Some(*pos),
                Dot::Released { .. } => None,
            })
            .collect()
    }

    pub fn advance_animations(&mut self) {
        // Active dots persist while the finger is down; released dots are
        // dropped once their fade is finished.
        self.dots.retain(|_, dot| match dot {
            Dot::Active { .. } => true,
            Dot::Released { fade, .. } => !fade.is_clamped_done(),
        });
    }

    pub fn are_animations_ongoing(&self) -> bool {
        self.dots.values().any(|dot| match dot {
            Dot::Active { appear, .. } => !appear.is_clamped_done(),
            Dot::Released { fade, .. } => !fade.is_clamped_done(),
        })
    }

    fn active_count(&self) -> usize {
        self.dots
            .values()
            .filter(|dot| matches!(dot, Dot::Active { .. }))
            .count()
    }

    /// Dots that should render under the given mode, as (position, alpha).
    ///
    /// In [`ShowTouchPoints::Gestures`] mode only qualified dots render —
    /// ones that were on screen while enough fingers were down. A dot that
    /// never qualified must not pop in at lift just to fade out.
    pub fn renderable_dots(&self, mode: ShowTouchPoints) -> Vec<(Point<f64, Logical>, f64)> {
        match mode {
            ShowTouchPoints::Off => Vec::new(),
            ShowTouchPoints::All => self
                .dots
                .values()
                .map(|dot| match dot {
                    Dot::Active {
                        pos, appear, ..
                    } => (*pos, appear.value()),
                    Dot::Released { pos, fade, .. } => (*pos, fade.value()),
                })
                .collect(),
            ShowTouchPoints::Gestures => self
                .dots
                .values()
                .filter_map(|dot| match dot {
                    Dot::Active {
                        pos,
                        appear,
                        qualified: true,
                    } => Some((*pos, appear.value())),
                    Dot::Released {
                        pos,
                        fade,
                        qualified: true,
                    } => Some((*pos, fade.value())),
                    _ => None,
                })
                .collect(),
        }
    }

    pub fn render<R: NiriRenderer>(
        &self,
        renderer: &mut R,
        output: &Output,
        output_geometry: Rectangle<f64, Logical>,
        mode: ShowTouchPoints,
        push: &mut dyn FnMut(PrimaryGpuTextureRenderElement),
    ) {
        if self.dots.is_empty() {
            return;
        }

        let scale = output.current_scale().fractional_scale();

        let mut buffers = self.buffers.borrow_mut();
        let buffer = buffers
            .entry(NotNan::new(scale).unwrap())
            .or_insert_with(|| render_dot_texture(renderer.as_gles_renderer(), scale).ok())
            .clone();
        let Some(buffer) = buffer else {
            return;
        };

        let size = buffer.logical_size();
        let half = Point::from((size.w / 2., size.h / 2.));

        for (pos, alpha) in self.renderable_dots(mode) {
            // Touch positions are global logical; the dot belongs to the
            // output it lands on.
            let center = pos - output_geometry.loc;

            // Cull dots far outside this output. A touch maps to exactly one
            // output, so this only skips neighbours in a multi-monitor setup.
            let margin = DOT_DIAMETER;
            if center.x < -margin
                || center.y < -margin
                || center.x > output_geometry.size.w + margin
                || center.y > output_geometry.size.h + margin
            {
                continue;
            }

            let location = (center - half).to_physical_precise_round(scale).to_logical(scale);

            let elem = TextureRenderElement::from_texture_buffer(
                buffer.clone(),
                location,
                alpha as f32,
                None,
                None,
                Kind::Unspecified,
            );
            push(PrimaryGpuTextureRenderElement(elem));
        }
    }
}

/// Draws the dot: a white disc with a dark rim, readable on any background.
fn render_dot_texture(
    renderer: &mut GlesRenderer,
    scale: f64,
) -> anyhow::Result<TextureBuffer<GlesTexture>> {
    let _span = tracy_client::span!("touch_points::render_dot_texture");

    // A couple of physical pixels of room for the rim.
    let margin: i32 = 2;
    let diameter_px: i32 =
        to_physical_precise_round::<i32>(scale, DOT_DIAMETER).max(1) + margin * 2;

    let surface = ImageSurface::create(cairo::Format::ARgb32, diameter_px, diameter_px)?;
    let cr = cairo::Context::new(&surface)?;

    let center = f64::from(diameter_px) / 2.;
    let radius = f64::from(diameter_px - margin * 2) / 2.;

    // Dark rim.
    cr.set_source_rgba(0., 0., 0., 0.35);
    cr.arc(center, center, radius, 0., std::f64::consts::TAU);
    cr.fill()?;

    // White disc on top.
    cr.set_source_rgba(1., 1., 1., 0.7);
    cr.arc(center, center, radius - 2., 0., std::f64::consts::TAU);
    cr.fill()?;

    drop(cr);
    let data = surface.take_data().unwrap();
    let buffer = TextureBuffer::from_memory(
        renderer,
        &data,
        Fourcc::Argb8888,
        (diameter_px, diameter_px),
        false,
        scale,
        Transform::Normal,
        Vec::new(),
    )?;

    Ok(buffer)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn slot(id: u32) -> TouchSlot {
        TouchSlot::from(Some(id))
    }

    fn touch_points_at(ms: u64) -> (Clock, TouchPoints) {
        let clock = Clock::with_time(Duration::from_millis(ms));
        let tp = TouchPoints::new(clock.clone());
        (clock, tp)
    }

    #[test]
    fn down_shows_dot_and_up_fades_it_out() {
        let (mut clock, mut tp) = touch_points_at(0);

        tp.down(slot(1), Point::from((100., 200.)), 3);
        assert!(!tp.is_empty());
        assert!(tp.are_animations_ongoing());

        // Well past the fade-out duration after lifting.
        clock.set_unadjusted(Duration::from_millis(1000));
        tp.up(slot(1));
        assert!(!tp.is_empty(), "released dot lingers while fading");
        assert!(
            tp.active_positions().is_empty(),
            "a lifted finger is no longer an active contact"
        );

        clock.set_unadjusted(Duration::from_millis(2000));
        tp.advance_animations();
        assert!(tp.is_empty(), "dot removed once the fade finishes");
    }

    #[test]
    fn motion_moves_only_active_dots() {
        let (mut clock, mut tp) = touch_points_at(0);

        tp.down(slot(1), Point::from((0., 0.)), 3);
        tp.motion(slot(1), Point::from((50., 60.)), 3);

        clock.set_unadjusted(Duration::from_millis(500));
        tp.up(slot(1));

        // Motion after lift must not move the fading dot.
        tp.motion(slot(1), Point::from((999., 999.)), 3);
        let Dot::Released { pos, .. } = tp.dots.get(&slot(1)).unwrap() else {
            panic!("expected a released dot");
        };
        assert_eq!(*pos, Point::from((50., 60.)));

        // Unknown slots are ignored rather than creating a phantom dot.
        tp.motion(slot(7), Point::from((1., 1.)), 3);
        assert!(!tp.dots.contains_key(&slot(7)));
    }

    #[test]
    fn appear_animation_settles() {
        let (mut clock, mut tp) = touch_points_at(0);

        tp.down(slot(1), Point::from((10., 10.)), 3);
        assert!(tp.are_animations_ongoing());

        clock.set_unadjusted(Duration::from_millis(500));
        assert!(!tp.are_animations_ongoing(), "appear finished");

        // The dot stays while the finger is down.
        tp.advance_animations();
        assert!(!tp.is_empty());
    }

    #[test]
    fn cancel_fades_everything() {
        let (mut clock, mut tp) = touch_points_at(0);

        tp.down(slot(1), Point::from((0., 0.)), 3);
        tp.down(slot(2), Point::from((10., 10.)), 3);

        clock.set_unadjusted(Duration::from_millis(100));
        tp.cancel();
        assert_eq!(tp.dots.len(), 2, "cancelled dots fade rather than vanish");

        clock.set_unadjusted(Duration::from_millis(1000));
        tp.advance_animations();
        assert!(tp.is_empty());
    }

    #[test]
    fn gestures_mode_needs_enough_fingers() {
        let (mut clock, mut tp) = touch_points_at(0);

        // Two fingers down: below the gesture threshold, no dots render.
        tp.down(slot(1), Point::from((0., 0.)), 3);
        tp.down(slot(2), Point::from((10., 10.)), 3);
        clock.set_unadjusted(Duration::from_millis(500));
        assert!(tp.renderable_dots(ShowTouchPoints::Gestures).is_empty());

        // Third finger joins: now all three render, including the two early
        // ones that started below the threshold.
        tp.down(slot(3), Point::from((20., 20.)), 3);
        assert_eq!(tp.renderable_dots(ShowTouchPoints::Gestures).len(), 3);

        // All mode shows everything, even a single finger.
        let (mut clock, mut tp2) = touch_points_at(0);
        tp2.down(slot(1), Point::from((0., 0.)), 3);
        clock.set_unadjusted(Duration::from_millis(500));
        assert_eq!(tp2.renderable_dots(ShowTouchPoints::All).len(), 1);

        // Off mode never renders.
        assert!(tp2.renderable_dots(ShowTouchPoints::Off).is_empty());
    }

    #[test]
    fn unqualified_dot_does_not_appear_at_lift() {
        let (mut clock, mut tp) = touch_points_at(0);

        // One finger touches and moves: never enough for a gesture.
        tp.down(slot(1), Point::from((0., 0.)), 3);
        tp.motion(slot(1), Point::from((50., 60.)), 3);
        assert!(tp.renderable_dots(ShowTouchPoints::Gestures).is_empty());

        // Regression: lifting an invisible dot must not flash it on screen
        // just to fade out.
        clock.set_unadjusted(Duration::from_millis(500));
        tp.up(slot(1));
        assert!(tp.renderable_dots(ShowTouchPoints::Gestures).is_empty());
    }

    #[test]
    fn released_dots_finish_fading_in_gestures_mode() {
        let (mut clock, mut tp) = touch_points_at(0);

        tp.down(slot(1), Point::from((0., 0.)), 3);
        tp.down(slot(2), Point::from((10., 10.)), 3);
        tp.down(slot(3), Point::from((20., 20.)), 3);
        clock.set_unadjusted(Duration::from_millis(500));

        // Two lift: the lifted dots were on screen during the gesture, so
        // they keep rendering while they fade out. The finger still down was
        // part of the gesture too, so it stays visible until it lifts.
        tp.up(slot(1));
        tp.up(slot(2));
        let visible = tp.renderable_dots(ShowTouchPoints::Gestures);
        assert_eq!(
            visible.len(),
            3,
            "two fading dots plus the finger still down"
        );

        clock.set_unadjusted(Duration::from_millis(2000));
        tp.up(slot(3));

        // Past every fade duration: all three dots are gone.
        clock.set_unadjusted(Duration::from_millis(3000));
        tp.advance_animations();
        assert!(tp.is_empty());
    }
}
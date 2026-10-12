use std::str::FromStr;

use miette::miette;
use smithay::input::keyboard::XkbConfig;
use smithay::reexports::input;

use crate::binds::{Modifiers, MIN_FINGERS};
use crate::utils::{Flag, MergeWith, Percent};
use crate::FloatOrInt;

#[derive(Debug, Default, PartialEq)]
pub struct Input {
    pub keyboard: Keyboard,
    pub touchpad: Touchpad,
    pub mouse: Mouse,
    pub trackpoint: Trackpoint,
    pub trackball: Trackball,
    pub tablet: Tablet,
    pub touchscreen: Touchscreen,
    pub disable_power_key_handling: bool,
    pub warp_mouse_to_focus: Option<WarpMouseToFocus>,
    pub focus_follows_mouse: Option<FocusFollowsMouse>,
    pub workspace_auto_back_and_forth: bool,
    pub mod_key: Option<ModKey>,
    pub mod_key_nested: Option<ModKey>,
}

#[derive(knuffel::Decode, Debug, Default, PartialEq)]
pub struct InputPart {
    #[knuffel(child)]
    pub keyboard: Option<KeyboardPart>,
    #[knuffel(child)]
    pub touchpad: Option<Touchpad>,
    #[knuffel(child)]
    pub mouse: Option<Mouse>,
    #[knuffel(child)]
    pub trackpoint: Option<Trackpoint>,
    #[knuffel(child)]
    pub trackball: Option<Trackball>,
    #[knuffel(child)]
    pub tablet: Option<Tablet>,
    #[knuffel(child)]
    pub touchscreen: Option<Touchscreen>,
    #[knuffel(child)]
    pub disable_power_key_handling: Option<Flag>,
    #[knuffel(child)]
    pub warp_mouse_to_focus: Option<WarpMouseToFocus>,
    #[knuffel(child)]
    pub focus_follows_mouse: Option<FocusFollowsMouse>,
    #[knuffel(child)]
    pub workspace_auto_back_and_forth: Option<Flag>,
    #[knuffel(child, unwrap(argument, str))]
    pub mod_key: Option<ModKey>,
    #[knuffel(child, unwrap(argument, str))]
    pub mod_key_nested: Option<ModKey>,
}

impl MergeWith<InputPart> for Input {
    fn merge_with(&mut self, part: &InputPart) {
        merge!(
            (self, part),
            keyboard,
            disable_power_key_handling,
            workspace_auto_back_and_forth,
        );

        merge_clone!(
            (self, part),
            touchpad,
            mouse,
            trackpoint,
            trackball,
            tablet,
            touchscreen,
        );

        merge_clone_opt!(
            (self, part),
            warp_mouse_to_focus,
            focus_follows_mouse,
            mod_key,
            mod_key_nested,
        );
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Keyboard {
    pub xkb: Xkb,
    pub repeat_delay: u16,
    pub repeat_rate: u8,
    pub track_layout: TrackLayout,
    pub numlock: bool,
}

impl Default for Keyboard {
    fn default() -> Self {
        Self {
            xkb: Default::default(),
            // The defaults were chosen to match wlroots and sway.
            repeat_delay: 600,
            repeat_rate: 25,
            track_layout: Default::default(),
            numlock: Default::default(),
        }
    }
}

#[derive(knuffel::Decode, Debug, PartialEq, Eq)]
pub struct KeyboardPart {
    #[knuffel(child)]
    pub xkb: Option<Xkb>,
    #[knuffel(child, unwrap(argument))]
    pub repeat_delay: Option<u16>,
    #[knuffel(child, unwrap(argument))]
    pub repeat_rate: Option<u8>,
    #[knuffel(child, unwrap(argument))]
    pub track_layout: Option<TrackLayout>,
    #[knuffel(child)]
    pub numlock: Option<Flag>,
}

impl MergeWith<KeyboardPart> for Keyboard {
    fn merge_with(&mut self, part: &KeyboardPart) {
        merge_clone!((self, part), xkb, repeat_delay, repeat_rate, track_layout);
        merge!((self, part), numlock);
    }
}

#[derive(knuffel::Decode, Debug, Default, PartialEq, Eq, Clone)]
pub struct Xkb {
    #[knuffel(child, unwrap(argument), default)]
    pub rules: String,
    #[knuffel(child, unwrap(argument), default)]
    pub model: String,
    #[knuffel(child, unwrap(argument), default)]
    pub layout: String,
    #[knuffel(child, unwrap(argument), default)]
    pub variant: String,
    #[knuffel(child, unwrap(argument))]
    pub options: Option<String>,
    #[knuffel(child, unwrap(argument))]
    pub file: Option<String>,
}

impl Xkb {
    pub fn to_xkb_config(&self) -> XkbConfig<'_> {
        XkbConfig {
            rules: &self.rules,
            model: &self.model,
            layout: &self.layout,
            variant: &self.variant,
            options: self.options.clone(),
        }
    }
}

#[derive(knuffel::DecodeScalar, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum TrackLayout {
    /// The layout change is global.
    #[default]
    Global,
    /// The layout change is window local.
    Window,
}

#[derive(knuffel::Decode, Debug, Default, Clone, Copy, PartialEq)]
pub struct ScrollFactor {
    #[knuffel(argument)]
    pub base: Option<FloatOrInt<0, 100>>,
    #[knuffel(property)]
    pub horizontal: Option<FloatOrInt<-100, 100>>,
    #[knuffel(property)]
    pub vertical: Option<FloatOrInt<-100, 100>>,
}

impl ScrollFactor {
    pub fn h_v_factors(&self) -> (f64, f64) {
        let base_value = self.base.map(|f| f.0).unwrap_or(1.0);
        let h = self.horizontal.map(|f| f.0).unwrap_or(base_value);
        let v = self.vertical.map(|f| f.0).unwrap_or(base_value);
        (h, v)
    }
}

#[derive(knuffel::Decode, Debug, Default, Clone, PartialEq)]
pub struct Touchpad {
    #[knuffel(child)]
    pub off: bool,
    #[knuffel(child)]
    pub tap: bool,
    #[knuffel(child)]
    pub dwt: bool,
    #[knuffel(child)]
    pub dwtp: bool,
    #[knuffel(child, unwrap(argument))]
    pub drag: Option<bool>,
    #[knuffel(child)]
    pub drag_lock: bool,
    #[knuffel(child)]
    pub natural_scroll: bool,
    #[knuffel(child, unwrap(argument, str))]
    pub click_method: Option<ClickMethod>,
    #[knuffel(child, unwrap(argument), default)]
    pub accel_speed: FloatOrInt<-1, 1>,
    #[knuffel(child, unwrap(argument, str))]
    pub accel_profile: Option<AccelProfile>,
    #[knuffel(child, unwrap(argument, str))]
    pub scroll_method: Option<ScrollMethod>,
    #[knuffel(child, unwrap(argument))]
    pub scroll_button: Option<u32>,
    #[knuffel(child)]
    pub scroll_button_lock: bool,
    #[knuffel(child, unwrap(argument, str))]
    pub tap_button_map: Option<TapButtonMap>,
    #[knuffel(child)]
    pub left_handed: bool,
    #[knuffel(child)]
    pub disabled_on_external_mouse: bool,
    #[knuffel(child)]
    pub middle_emulation: bool,
    #[knuffel(child)]
    pub scroll_factor: Option<ScrollFactor>,
    #[knuffel(child, unwrap(argument))]
    pub pinch_sensitivity: Option<FloatOrInt<0, 100>>,
}

#[derive(knuffel::Decode, Debug, Default, Clone, PartialEq)]
pub struct Mouse {
    #[knuffel(child)]
    pub off: bool,
    #[knuffel(child)]
    pub natural_scroll: bool,
    #[knuffel(child, unwrap(argument), default)]
    pub accel_speed: FloatOrInt<-1, 1>,
    #[knuffel(child, unwrap(argument, str))]
    pub accel_profile: Option<AccelProfile>,
    #[knuffel(child, unwrap(argument, str))]
    pub scroll_method: Option<ScrollMethod>,
    #[knuffel(child, unwrap(argument))]
    pub scroll_button: Option<u32>,
    #[knuffel(child)]
    pub scroll_button_lock: bool,
    #[knuffel(child)]
    pub left_handed: bool,
    #[knuffel(child)]
    pub middle_emulation: bool,
    #[knuffel(child)]
    pub scroll_factor: Option<ScrollFactor>,
}

#[derive(knuffel::Decode, Debug, Default, Clone, PartialEq)]
pub struct Trackpoint {
    #[knuffel(child)]
    pub off: bool,
    #[knuffel(child)]
    pub natural_scroll: bool,
    #[knuffel(child, unwrap(argument), default)]
    pub accel_speed: FloatOrInt<-1, 1>,
    #[knuffel(child, unwrap(argument, str))]
    pub accel_profile: Option<AccelProfile>,
    #[knuffel(child, unwrap(argument, str))]
    pub scroll_method: Option<ScrollMethod>,
    #[knuffel(child, unwrap(argument))]
    pub scroll_button: Option<u32>,
    #[knuffel(child)]
    pub scroll_button_lock: bool,
    #[knuffel(child)]
    pub left_handed: bool,
    #[knuffel(child)]
    pub middle_emulation: bool,
}

#[derive(knuffel::Decode, Debug, Default, Clone, PartialEq)]
pub struct Trackball {
    #[knuffel(child)]
    pub off: bool,
    #[knuffel(child)]
    pub natural_scroll: bool,
    #[knuffel(child, unwrap(argument), default)]
    pub accel_speed: FloatOrInt<-1, 1>,
    #[knuffel(child, unwrap(argument, str))]
    pub accel_profile: Option<AccelProfile>,
    #[knuffel(child, unwrap(argument, str))]
    pub scroll_method: Option<ScrollMethod>,
    #[knuffel(child, unwrap(argument))]
    pub scroll_button: Option<u32>,
    #[knuffel(child)]
    pub scroll_button_lock: bool,
    #[knuffel(child)]
    pub left_handed: bool,
    #[knuffel(child)]
    pub middle_emulation: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClickMethod {
    Clickfinger,
    ButtonAreas,
}

impl From<ClickMethod> for input::ClickMethod {
    fn from(value: ClickMethod) -> Self {
        match value {
            ClickMethod::Clickfinger => Self::Clickfinger,
            ClickMethod::ButtonAreas => Self::ButtonAreas,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccelProfile {
    Adaptive,
    Flat,
}

impl From<AccelProfile> for input::AccelProfile {
    fn from(value: AccelProfile) -> Self {
        match value {
            AccelProfile::Adaptive => Self::Adaptive,
            AccelProfile::Flat => Self::Flat,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollMethod {
    NoScroll,
    TwoFinger,
    Edge,
    OnButtonDown,
}

impl From<ScrollMethod> for input::ScrollMethod {
    fn from(value: ScrollMethod) -> Self {
        match value {
            ScrollMethod::NoScroll => Self::NoScroll,
            ScrollMethod::TwoFinger => Self::TwoFinger,
            ScrollMethod::Edge => Self::Edge,
            ScrollMethod::OnButtonDown => Self::OnButtonDown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TapButtonMap {
    LeftRightMiddle,
    LeftMiddleRight,
}

impl From<TapButtonMap> for input::TapButtonMap {
    fn from(value: TapButtonMap) -> Self {
        match value {
            TapButtonMap::LeftRightMiddle => Self::LeftRightMiddle,
            TapButtonMap::LeftMiddleRight => Self::LeftMiddleRight,
        }
    }
}

#[derive(knuffel::Decode, Debug, Default, Clone, PartialEq)]
pub struct Tablet {
    #[knuffel(child)]
    pub off: bool,
    #[knuffel(child, unwrap(arguments))]
    pub calibration_matrix: Option<Vec<f32>>,
    #[knuffel(child, unwrap(argument))]
    pub map_to_output: Option<String>,
    #[knuffel(child)]
    pub map_to_focused_output: bool,
    #[knuffel(child)]
    pub map_to_focused_window: bool,
    #[knuffel(child)]
    pub left_handed: bool,
}

/// Tuning parameters for touchscreen gesture recognition.
///
/// The actual gesture binds (e.g. `TouchSwipe fingers=3 direction="up"`,
/// `TouchEdge edge="left"`) live in the main `binds {}` block — this
/// struct only controls how movement is classified.
/// A run of `w h x y` quadruples, validated to be a whole number of rects
/// at DECODE time rather than at accessor time.
///
/// The length check cannot live in the `FloatOrInt` bound (it constrains
/// each element, not the run), so it is enforced by decoding the arguments
/// here. Doing it in `Decode` means `exclusion-rects 400 300 0 0 500` is a
/// load-time config error, like every other malformed value in niri,
/// instead of a `warn!` at gesture time that leaves the user staring at a
/// feature which silently does nothing.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExclusionRects(Vec<FloatOrInt<-1_000_000, 1_000_000>>);

impl ExclusionRects {
    /// The decoded values, in authored order.
    pub fn values(&self) -> &[FloatOrInt<-1_000_000, 1_000_000>] {
        &self.0
    }

    /// The values grouped into `w h x y` quadruples.
    ///
    /// `decode_node` guarantees the length is a multiple of four, so
    /// `chunks_exact` never silently drops a tail.
    pub fn rects(&self) -> impl Iterator<Item = (f64, f64, f64, f64)> + '_ {
        self.0
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| (c[0].0, c[1].0, c[2].0, c[3].0))
    }
}

impl std::ops::Deref for ExclusionRects {
    type Target = [FloatOrInt<-1_000_000, 1_000_000>];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl FromIterator<FloatOrInt<-1_000_000, 1_000_000>> for ExclusionRects {
    fn from_iter<T: IntoIterator<Item = FloatOrInt<-1_000_000, 1_000_000>>>(iter: T) -> Self {
        ExclusionRects(iter.into_iter().collect())
    }
}

impl<S: knuffel::traits::ErrorSpan> knuffel::Decode<S> for ExclusionRects {
    fn decode_node(
        node: &knuffel::ast::SpannedNode<S>,
        ctx: &mut knuffel::decode::Context<S>,
    ) -> Result<Self, knuffel::errors::DecodeError<S>> {
        use knuffel::traits::DecodeScalar;

        let mut values = Vec::with_capacity(node.arguments.len());
        for arg in &node.arguments {
            values.push(DecodeScalar::decode(arg, ctx)?);
        }

        if values.len() % 4 != 0 {
            ctx.emit_error(knuffel::errors::DecodeError::conversion(
                node,
                format!(
                    "exclusion-rects takes groups of four numbers \
                     (width height x y), but got {}",
                    values.len()
                ),
            ));
        }

        Ok(ExclusionRects(values))
    }
}

#[derive(knuffel::Decode, Debug, Default, Clone, PartialEq)]
pub struct TouchscreenGesturesConfig {
    /// How many simultaneous contacts arm the compositor when the sequence
    /// did not start in an edge zone or under Mod.
    ///
    /// Contacts below this count are forwarded to the client, so
    /// two-finger scroll/zoom passthrough keeps working. Raise it to make
    /// the compositor less trigger-happy at the cost of needing more
    /// fingers. Default: 3, the same floor the bind parser enforces
    /// (`MIN_FINGERS`).
    ///
    /// The floor is 2, not 1. `decide` step 8 gates arming on
    /// `min_gesture_fingers >= 2`, so a configured 1 would not mean "arm on
    /// one finger" — it would turn arming off entirely, and every
    /// mid-screen multi-finger swipe would silently stop working with no
    /// diagnostic. Two is the smallest value that leaves arming enabled, and
    /// it is also the value that keeps two-finger scroll/zoom passthrough
    /// intact, which is the whole point of the knob.
    #[knuffel(child, unwrap(argument))]
    pub min_gesture_fingers: Option<FloatOrInt<2, 10>>,
    /// Rectangles inside which the compositor's touchscreen gestures are
    /// suppressed and touches are always forwarded to the client.
    ///
    /// This is the L3 shape of Android's `systemGestureExclusionRects`: a
    /// region of the screen where a compositor gesture must not fire, so a
    /// player, a drawing app or a map can own the area without the
    /// compositor guessing from an app-id match.
    ///
    /// # Coordinate space
    ///
    /// **Global logical pixels**, in the same space as a touch-down's
    /// position: the origin is the top-left corner of the bounding box of
    /// all outputs, and each output contributes its logical size at its
    /// `position` from the `outputs {}` block. A rect is therefore NOT
    /// per-output, and one authored for a single-monitor layout will be in
    /// the wrong place once a second monitor is added to the left or above.
    ///
    /// Each rectangle is four arguments in the order `w h x y`, matching the
    /// Android field order and reading naturally as "a box this big at this
    /// position":
    ///
    /// ```kdl
    /// input {
    ///     touchscreen {
    ///         gestures {
    ///             exclusion-rects 1280 720 0 0 400 300 1700 0
    ///         }
    ///     }
    /// }
    /// ```
    ///
    /// It is ONE node carrying every rectangle as a run of
    /// `w h x y` quadruples, rather than a repeated node: niri's config
    /// grammar rejects duplicate child nodes, and a single node keeps the
    /// malformed-input case unambiguous — a run whose length is not a
    /// multiple of four is an error, not a partial last rectangle.
    ///
    /// Rects are half-open on the right and bottom edges, so two abutting
    /// rects partition the plane without both claiming the boundary.
    /// The lower bound is negative on purpose. The coordinate space is
    /// global logical pixels with its origin at the top-left of the bounding
    /// box of all outputs, so a monitor placed to the left of or above that
    /// box has negative x/y and cannot be described at all with a
    /// zero-based bound. The upper bound stays at 1_000_000 as a sanity
    /// limit; neither bound needs to grow to cover a 32K display.
    #[knuffel(child)]
    pub exclusion_rects: Option<ExclusionRects>,
    /// Swipe commit gate: pixels of centroid movement required before a
    /// swipe gesture latches. Lower values feel more responsive but risk
    /// triggering on incidental finger drift. Default: 100.0.
    #[knuffel(child, unwrap(argument))]
    pub swipe_trigger_distance: Option<FloatOrInt<0, 100_000>>,
    /// Width (in pixels) of the screen-edge start zone. A touch must
    /// *begin* within this distance from an edge for it to count as a
    /// `TouchEdge edge="..."` gesture; touches starting farther in are
    /// treated as regular swipes. Default: 12.0.
    #[knuffel(child, unwrap(argument))]
    pub edge_start_distance: Option<FloatOrInt<0, 100_000>>,
    /// Pinch commit gate: pixels of `|spread_change|` required before a
    /// pinch gesture latches. Default: 100.0.
    #[knuffel(child, unwrap(argument))]
    pub pinch_trigger_distance: Option<FloatOrInt<0, 100_000>>,
    /// Rotation commit gate: cumulative rotation (in **degrees**) required
    /// before a rotation gesture latches. Converted to radians
    /// internally. Default: 20.
    #[knuffel(child, unwrap(argument))]
    pub rotation_trigger_angle: Option<FloatOrInt<0, 360>>,
    /// Maximum per-finger displacement (in pixels) allowed during a tap
    /// gesture. If any single finger moves farther than this from its
    /// initial landing position, the tap candidate is killed and the
    /// gesture can only resolve as swipe/pinch/rotate. Default: 15.0.
    #[knuffel(child, unwrap(argument))]
    pub tap_wobble_threshold: Option<FloatOrInt<0, 100_000>>,
    /// Maximum duration (in milliseconds) from the first finger landing to
    /// all fingers lifting for a tap to fire. Taps slower than this are
    /// discarded. Default: 500.
    #[knuffel(child, unwrap(argument))]
    pub tap_timeout_ms: Option<FloatOrInt<0, 1_000_000>>,
}

/// A screen edge, as used by `TouchEdge` gesture binds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScreenEdge {
    Left,
    Right,
    Top,
    Bottom,
}

impl ScreenEdge {
    /// Lower-cased name used in KDL config (`"left"`, `"right"`, `"top"`,
    /// `"bottom"`).
    pub fn as_kdl_name(self) -> &'static str {
        match self {
            ScreenEdge::Left => "left",
            ScreenEdge::Right => "right",
            ScreenEdge::Top => "top",
            ScreenEdge::Bottom => "bottom",
        }
    }
}

/// Which third of an edge a touch landed in.
///
/// The perpendicular axis of the edge is split into thirds: for Top/Bottom
/// that's the x axis (Start=leftmost third, End=rightmost third); for
/// Left/Right that's the y axis (Start=topmost third, End=bottommost third).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EdgeZone {
    Start,
    Center,
    End,
}

/// Lower-cased zone name used in KDL config. The vocabulary rotates per
/// edge axis: top/bottom edges take `left|center|right`; left/right edges
/// take `top|center|bottom`. This is the single source of truth for that
/// mapping.
pub fn zone_kdl_name(edge: ScreenEdge, zone: EdgeZone) -> &'static str {
    match (edge, zone) {
        (ScreenEdge::Top | ScreenEdge::Bottom, EdgeZone::Start) => "left",
        (ScreenEdge::Top | ScreenEdge::Bottom, EdgeZone::Center) => "center",
        (ScreenEdge::Top | ScreenEdge::Bottom, EdgeZone::End) => "right",
        (ScreenEdge::Left | ScreenEdge::Right, EdgeZone::Start) => "top",
        (ScreenEdge::Left | ScreenEdge::Right, EdgeZone::Center) => "center",
        (ScreenEdge::Left | ScreenEdge::Right, EdgeZone::End) => "bottom",
    }
}

/// One of the four screen corners, as used by `Corner` gesture binds.
///
/// Mirrors `touch_core::Corner` (crates/touch-core/src/contact.rs:144) one
/// for one. The two enums stay separate on purpose: `touch-core` is a pure,
/// clock-free L1/L2 crate that must not depend on the config crate, so the
/// mapping is written out at the single site that crosses the boundary,
/// [`Trigger::Corner`](crate::binds::Trigger::Corner) construction in
/// `src/input/touch_arbitration_grab.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScreenCorner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

/// Lower-cased corner name used in KDL config (`"top-left"`, ...).
///
/// Unlike [`zone_kdl_name`] this mapping does not rotate per axis: a corner
/// names itself, so the four spellings are absolute.
pub fn corner_kdl_name(corner: ScreenCorner) -> &'static str {
    match corner {
        ScreenCorner::TopLeft => "top-left",
        ScreenCorner::TopRight => "top-right",
        ScreenCorner::BottomLeft => "bottom-left",
        ScreenCorner::BottomRight => "bottom-right",
    }
}

#[derive(knuffel::Decode, Debug, Default, Clone, PartialEq)]
pub struct Touchscreen {
    #[knuffel(child)]
    pub off: bool,
    #[knuffel(child, unwrap(arguments))]
    pub calibration_matrix: Option<Vec<f32>>,
    #[knuffel(child, unwrap(argument))]
    pub map_to_output: Option<String>,
    #[knuffel(child)]
    pub gestures: Option<TouchscreenGesturesConfig>,
}

impl Touchscreen {
    /// How many simultaneous contacts arm the compositor when the sequence
    /// did not start in an edge zone or under Mod.
    ///
    /// Defaults to `MIN_FINGERS` (3) so that any gesture the bind parser
    /// can express is also a gesture that can arm.
    pub fn min_gesture_fingers(&self) -> u32 {
        self.gestures
            .as_ref()
            .and_then(|g| g.min_gesture_fingers)
            // Round rather than truncate. The field is `FloatOrInt`, so
            // `min-gesture-fingers 3.7` parses; a truncating `as u32` turned
            // that into 3 with no diagnostic, so the user got a different
            // threshold from the one they wrote. Rounding is the nearest
            // whole number, which is the least surprising reading of a
            // finger count, and it is clamped to the same `2..=10` window the
            // type enforces so a value that somehow arrives out of range
            // cannot disable arming or demand an impossible hand.
            .map_or(MIN_FINGERS as u32, |v| (v.0.round() as u32).clamp(2, 10))
    }

    /// Gesture exclusion rectangles, as `(width, height, x, y)` tuples.
    ///
    /// Parsed from the flat argument list on each `exclusion-rects` child.
    ///
    /// A run whose length is not a multiple of four is rejected during
    /// config LOAD by `ExclusionRects::decode_node`, so a bad rect can no
    /// longer reach here from a config file. The `Result` is retained
    /// deliberately: it is part of the public signature the compositor
    /// adapter already matches on, and keeping it means that guarantee does
    /// not depend on this file being the only way to build an
    /// `ExclusionRects`. The check below is therefore defence in depth, not
    /// the primary path.
    ///
    /// **Coordinate space: global logical pixels.** Same space as a
    /// touch-down's position — the origin is the top-left of the bounding
    /// box of all outputs, with each output contributing its logical size at
    /// its configured `position`. Not per-output, so a rect authored for a
    /// single-monitor layout will be in the wrong place once a second
    /// monitor is added to the left or above.
    pub fn exclusion_rects(&self) -> Result<Vec<(f64, f64, f64, f64)>, String> {
        let Some(rects) = self
            .gestures
            .as_ref()
            .and_then(|g| g.exclusion_rects.as_ref())
        else {
            return Ok(Vec::new());
        };

        if rects.values().len() % 4 != 0 {
            return Err(format!(
                "exclusion-rects takes groups of four numbers (width height x y), \
                 but got {}",
                rects.values().len()
            ));
        }

        Ok(rects.rects().collect())
    }

    /// Swipe commit gate: centroid must travel this many pixels before a
    /// swipe can latch. Default 100.
    pub fn swipe_trigger_distance(&self) -> f64 {
        self.gestures
            .as_ref()
            .and_then(|g| g.swipe_trigger_distance)
            .map(|v| v.0)
            .unwrap_or(100.0)
    }

    /// Width (in pixels) of the screen-edge start zone within which a touch
    /// must begin to count as a `TouchEdge`. Default 12.
    pub fn edge_start_distance(&self) -> f64 {
        self.gestures
            .as_ref()
            .and_then(|g| g.edge_start_distance)
            .map(|v| v.0)
            .unwrap_or(12.0)
    }

    /// Pinch commit gate: `|spread_change|` must exceed this many pixels
    /// before a pinch can latch. Default 100.
    pub fn pinch_trigger_distance(&self) -> f64 {
        self.gestures
            .as_ref()
            .and_then(|g| g.pinch_trigger_distance)
            .map(|v| v.0)
            .unwrap_or(100.0)
    }
    /// Rotation commit gate in **radians**, converted from the
    /// degree-valued `rotation-trigger-angle` config knob. Default 20.
    pub fn rotation_trigger_angle(&self) -> f64 {
        let deg = self
            .gestures
            .as_ref()
            .and_then(|g| g.rotation_trigger_angle)
            .map(|v| v.0)
            .unwrap_or(20.0);
        deg.to_radians()
    }
    /// Maximum per-finger displacement (px) before a tap candidate is
    /// killed. Default 15.
    pub fn tap_wobble_threshold(&self) -> f64 {
        self.gestures
            .as_ref()
            .and_then(|g| g.tap_wobble_threshold)
            .map(|v| v.0)
            .unwrap_or(15.0)
    }

    /// Maximum tap duration in milliseconds. Default 500.
    pub fn tap_timeout_ms(&self) -> f64 {
        self.gestures
            .as_ref()
            .and_then(|g| g.tap_timeout_ms)
            .map(|v| v.0)
            .unwrap_or(500.0)
    }
}

#[derive(knuffel::Decode, Debug, Clone, Copy, PartialEq)]
pub struct FocusFollowsMouse {
    #[knuffel(property, str)]
    pub max_scroll_amount: Option<Percent>,
}

#[derive(knuffel::Decode, Debug, PartialEq, Eq, Clone, Copy)]
pub struct WarpMouseToFocus {
    #[knuffel(property, str)]
    pub mode: Option<WarpMouseToFocusMode>,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum WarpMouseToFocusMode {
    CenterXy,
    CenterXyAlways,
}

impl FromStr for WarpMouseToFocusMode {
    type Err = miette::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "center-xy" => Ok(Self::CenterXy),
            "center-xy-always" => Ok(Self::CenterXyAlways),
            _ => Err(miette!(
                r#"invalid mode for warp-mouse-to-focus, can be "center-xy" or "center-xy-always" (or leave unset for separate centering)"#
            )),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum ModKey {
    Ctrl,
    Shift,
    Alt,
    Super,
    IsoLevel3Shift,
    IsoLevel5Shift,
}

impl ModKey {
    pub fn to_modifiers(&self) -> Modifiers {
        match self {
            ModKey::Ctrl => Modifiers::CTRL,
            ModKey::Shift => Modifiers::SHIFT,
            ModKey::Alt => Modifiers::ALT,
            ModKey::Super => Modifiers::SUPER,
            ModKey::IsoLevel3Shift => Modifiers::ISO_LEVEL3_SHIFT,
            ModKey::IsoLevel5Shift => Modifiers::ISO_LEVEL5_SHIFT,
        }
    }
}

impl FromStr for ModKey {
    type Err = miette::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match &*s.to_ascii_lowercase() {
            "ctrl" | "control" => Ok(Self::Ctrl),
            "shift" => Ok(Self::Shift),
            "alt" => Ok(Self::Alt),
            "super" | "win" => Ok(Self::Super),
            "iso_level3_shift" | "mod5" => Ok(Self::IsoLevel3Shift),
            "iso_level5_shift" | "mod3" => Ok(Self::IsoLevel5Shift),
            _ => Err(miette!("invalid Mod key: {s}")),
        }
    }
}

impl FromStr for ClickMethod {
    type Err = miette::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "clickfinger" => Ok(Self::Clickfinger),
            "button-areas" => Ok(Self::ButtonAreas),
            _ => Err(miette!(
                r#"invalid click method, can be "button-areas" or "clickfinger""#
            )),
        }
    }
}

impl FromStr for AccelProfile {
    type Err = miette::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "adaptive" => Ok(Self::Adaptive),
            "flat" => Ok(Self::Flat),
            _ => Err(miette!(
                r#"invalid accel profile, can be "adaptive" or "flat""#
            )),
        }
    }
}

impl FromStr for ScrollMethod {
    type Err = miette::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "no-scroll" => Ok(Self::NoScroll),
            "two-finger" => Ok(Self::TwoFinger),
            "edge" => Ok(Self::Edge),
            "on-button-down" => Ok(Self::OnButtonDown),
            _ => Err(miette!(
                r#"invalid scroll method, can be "no-scroll", "two-finger", "edge", or "on-button-down""#
            )),
        }
    }
}

impl FromStr for TapButtonMap {
    type Err = miette::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "left-right-middle" => Ok(Self::LeftRightMiddle),
            "left-middle-right" => Ok(Self::LeftMiddleRight),
            _ => Err(miette!(
                r#"invalid tap button map, can be "left-right-middle" or "left-middle-right""#
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use insta::assert_debug_snapshot;

    use super::*;

    #[track_caller]
    fn do_parse(text: &str) -> Input {
        let part = knuffel::parse("test.kdl", text)
            .map_err(miette::Report::new)
            .unwrap();
        Input::from_part(&part)
    }

    fn parse_exclusion_rects(text: &str) -> Input {
        do_parse(&format!(
            r#"
            touchscreen {{
                gestures {{
                    {text}
                }}
            }}
            "#
        ))
    }

    const GESTURES_ONE: &str = r#"
        touchscreen { gestures { min-gesture-fingers 1 } }
        "#;
    const GESTURES_ELEVEN: &str = r#"
        touchscreen { gestures { min-gesture-fingers 11 } }
        "#;

    /// The floor and ceiling are both enforced, and 2 still works.
    #[test]
    fn min_gesture_fingers_bounds() {
        for ok in [
            "min-gesture-fingers 2",
            "min-gesture-fingers 3",
            "min-gesture-fingers 10",
        ] {
            let input = parse_exclusion_rects(ok);
            assert!(
                input.touchscreen.min_gesture_fingers() > 0,
                "{ok} should parse"
            );
        }

        // 1 is below the floor: it is not merely clamped, it must not parse
        // at all, because arming is gated on `min_gesture_fingers >= 2` and a
        // silently-accepted 1 would turn every mid-screen gesture off with no
        // diagnostic.
        assert!(
            knuffel::parse("test.kdl", GESTURES_ONE)
                .map(|part| Input::from_part(&part))
                .is_err(),
            "min-gesture-fingers 1 is below the floor and must not parse"
        );

        assert!(
            knuffel::parse("test.kdl", GESTURES_ELEVEN)
                .map(|part| Input::from_part(&part))
                .is_err(),
            "min-gesture-fingers 11 is above the ceiling and must not parse"
        );
    }

    /// A non-integer value must round to the nearest whole finger count.
    ///
    /// The field is `FloatOrInt`, so `3.7` parses. It used to be truncated
    /// by `v.0 as u32`, silently turning 3.7 into 3 — the user got a
    /// different threshold from the one they wrote, with no diagnostic.
    #[test]
    fn min_gesture_fingers_rounds_rather_than_truncates() {
        let three_seven = parse_exclusion_rects("min-gesture-fingers 3.7");
        assert_eq!(
            three_seven.touchscreen.min_gesture_fingers(),
            4,
            "3.7 must round to 4, not truncate to 3"
        );

        // And a value that rounds below the floor must not disable arming.
        let two_one = parse_exclusion_rects("min-gesture-fingers 2.1");
        assert_eq!(two_one.touchscreen.min_gesture_fingers(), 2);

        // An unset knob keeps the bind-parser floor.
        assert_eq!(
            parse_exclusion_rects("exclusion-rects 1 1 0 0")
                .touchscreen
                .min_gesture_fingers(),
            MIN_FINGERS as u32
        );
    }

    #[test]
    fn exclusion_rects_parse_a_single_rect() {
        let input = parse_exclusion_rects("exclusion-rects 1280 720 0 0");
        assert_eq!(
            input.touchscreen.exclusion_rects().unwrap(),
            vec![(1280.0, 720.0, 0.0, 0.0)]
        );
    }

    #[test]
    fn exclusion_rects_parse_multiple_rects_in_order() {
        let input = parse_exclusion_rects("exclusion-rects 1280 720 0 0 400 300 1700 0");
        assert_eq!(
            input.touchscreen.exclusion_rects().unwrap(),
            vec![(1280.0, 720.0, 0.0, 0.0), (400.0, 300.0, 1700.0, 0.0)]
        );
    }

    #[test]
    fn exclusion_rects_default_to_empty_when_unset() {
        let input = parse_exclusion_rects("min-gesture-fingers 3");
        assert!(input.touchscreen.exclusion_rects().unwrap().is_empty());
    }

    #[test]
    fn exclusion_rects_reject_a_partial_rect_at_parse_time() {
        // A three-number run is not a rectangle. This must fail the CONFIG
        // LOAD, not surface later as a runtime warning: a silently-ignored
        // exclusion rect means a video player keeps eating the user's edge
        // gestures with no symptom at all, and niri otherwise starts up
        // looking perfectly healthy.
        let result = knuffel::parse::<InputPart>(
            "test.kdl",
            r#"
            touchscreen {
                gestures {
                    exclusion-rects 1280 720 0
                }
            }
            "#,
        );
        assert!(result.is_err(), "partial rect must be a load-time error");
        let msg = format!("{result:?}");
        assert!(
            msg.contains("groups of four") && msg.contains("got 3"),
            "unhelpful error: {msg}"
        );
    }

    #[test]
    fn exclusion_rects_accept_a_negative_x() {
        // A monitor left of the origin has negative global logical x.
        let input = parse_exclusion_rects("exclusion-rects 400 300 -100 0");
        assert_eq!(
            input.touchscreen.exclusion_rects().unwrap(),
            vec![(400.0, 300.0, -100.0, 0.0)]
        );
    }

    #[test]
    fn exclusion_rects_accept_a_negative_y() {
        // A monitor above the origin has negative global logical y.
        let input = parse_exclusion_rects("exclusion-rects 400 300 0 -50");
        assert_eq!(
            input.touchscreen.exclusion_rects().unwrap(),
            vec![(400.0, 300.0, 0.0, -50.0)]
        );
    }

    #[test]
    fn exclusion_rects_accept_a_rect_with_both_coordinates_negative() {
        // The upper-left monitor case: both x and y negative at once.
        let input = parse_exclusion_rects("exclusion-rects 1920 1080 -1920 -1080");
        assert_eq!(
            input.touchscreen.exclusion_rects().unwrap(),
            vec![(1920.0, 1080.0, -1920.0, -1080.0)]
        );
    }

    #[test]
    fn exclusion_rects_accept_negative_and_positive_rects_together() {
        // A left-hand monitor and a right-hand one in the same run: the
        // ordering of the quadruples must survive the signed bounds.
        let input = parse_exclusion_rects("exclusion-rects 1920 1080 -1920 0 1920 1080 0 0");
        assert_eq!(
            input.touchscreen.exclusion_rects().unwrap(),
            vec![(1920.0, 1080.0, -1920.0, 0.0), (1920.0, 1080.0, 0.0, 0.0)]
        );
    }

    #[test]
    fn exclusion_rects_accept_negative_floating_point_coordinates() {
        // The signed bound must apply to the Decimal path too, not just Int.
        let input = parse_exclusion_rects("exclusion-rects 1920.5 1080.5 -1920.5 -1080.5");
        assert_eq!(
            input.touchscreen.exclusion_rects().unwrap(),
            vec![(1920.5, 1080.5, -1920.5, -1080.5)]
        );
    }

    #[test]
    fn exclusion_rects_still_reject_out_of_range_values() {
        // Widening the lower bound must not have removed the upper bound:
        // 2_000_000 is still absurd for a pixel coordinate and stays an
        // error. The negative side is bounded too, so a typo like
        // -2_000_000 cannot silently become a rect off in space.
        for bad in ["2000000", "-2000000"] {
            let result = knuffel::parse::<InputPart>(
                "test.kdl",
                &format!(
                    r#"
                    touchscreen {{
                        gestures {{
                            exclusion-rects 1280 720 {bad} 0
                        }}
                    }}
                    "#
                ),
            );
            assert!(result.is_err(), "value {bad} should be rejected");
            let msg = format!("{result:?}");
            assert!(
                msg.contains("value must be between -1000000 and 1000000"),
                "unhelpful error for {bad}: {msg}"
            );
        }
    }

    #[test]
    fn exclusion_rects_reject_a_non_numeric_argument() {
        // The turbofish is required and is not a behaviour change: `do_parse`
        // above infers `T` from the `Input::from_part(&part)` call that
        // follows its parse. This test asserts on the `Err` and never builds
        // an `Input`, so there is nothing to infer from and the type must be
        // named. Same function, same arguments, same decoder.
        let result = knuffel::parse::<InputPart>(
            "test.kdl",
            r#"
            touchscreen {
                gestures {
                    exclusion-rects 1280 720 oops 0
                }
            }
            "#,
        );
        assert!(result.is_err(), "non-numeric rect argument was accepted");
    }

    #[test]
    fn exclusion_rects_accept_floating_point_coordinates() {
        let input = parse_exclusion_rects("exclusion-rects 1920.5 1080.5 0.5 0.5");
        assert_eq!(
            input.touchscreen.exclusion_rects().unwrap(),
            vec![(1920.5, 1080.5, 0.5, 0.5)]
        );
    }

    #[test]
    fn parse_scroll_factor_combined() {
        // Test combined scroll-factor syntax
        let parsed = do_parse(
            r#"
            mouse {
                scroll-factor 2.0
            }
            touchpad {
                scroll-factor 1.5
            }
            "#,
        );

        assert_debug_snapshot!(parsed.mouse.scroll_factor, @r#"
        Some(
            ScrollFactor {
                base: Some(
                    FloatOrInt(
                        2.0,
                    ),
                ),
                horizontal: None,
                vertical: None,
            },
        )
        "#);
        assert_debug_snapshot!(parsed.touchpad.scroll_factor, @r#"
        Some(
            ScrollFactor {
                base: Some(
                    FloatOrInt(
                        1.5,
                    ),
                ),
                horizontal: None,
                vertical: None,
            },
        )
        "#);
    }

    #[test]
    fn parse_scroll_factor_split() {
        // Test split horizontal/vertical syntax
        let parsed = do_parse(
            r#"
            mouse {
                scroll-factor horizontal=2.0 vertical=-1.0
            }
            touchpad {
                scroll-factor horizontal=-1.5 vertical=0.5
            }
            "#,
        );

        assert_debug_snapshot!(parsed.mouse.scroll_factor, @r#"
        Some(
            ScrollFactor {
                base: None,
                horizontal: Some(
                    FloatOrInt(
                        2.0,
                    ),
                ),
                vertical: Some(
                    FloatOrInt(
                        -1.0,
                    ),
                ),
            },
        )
        "#);
        assert_debug_snapshot!(parsed.touchpad.scroll_factor, @r#"
        Some(
            ScrollFactor {
                base: None,
                horizontal: Some(
                    FloatOrInt(
                        -1.5,
                    ),
                ),
                vertical: Some(
                    FloatOrInt(
                        0.5,
                    ),
                ),
            },
        )
        "#);
    }

    #[test]
    fn parse_scroll_factor_partial() {
        // Test partial specification (only one axis)
        let parsed = do_parse(
            r#"
            mouse {
                scroll-factor horizontal=2.0
            }
            touchpad {
                scroll-factor vertical=-1.5
            }
            "#,
        );

        assert_debug_snapshot!(parsed.mouse.scroll_factor, @r#"
        Some(
            ScrollFactor {
                base: None,
                horizontal: Some(
                    FloatOrInt(
                        2.0,
                    ),
                ),
                vertical: None,
            },
        )
        "#);
        assert_debug_snapshot!(parsed.touchpad.scroll_factor, @r#"
        Some(
            ScrollFactor {
                base: None,
                horizontal: None,
                vertical: Some(
                    FloatOrInt(
                        -1.5,
                    ),
                ),
            },
        )
        "#);
    }

    #[test]
    fn parse_scroll_factor_mixed() {
        // Test mixed base + override syntax
        let parsed = do_parse(
            r#"
            mouse {
                scroll-factor 2 vertical=-1
            }
            touchpad {
                scroll-factor 1.5 horizontal=3
            }
            "#,
        );

        assert_debug_snapshot!(parsed.mouse.scroll_factor, @r#"
        Some(
            ScrollFactor {
                base: Some(
                    FloatOrInt(
                        2.0,
                    ),
                ),
                horizontal: None,
                vertical: Some(
                    FloatOrInt(
                        -1.0,
                    ),
                ),
            },
        )
        "#);
        assert_debug_snapshot!(parsed.touchpad.scroll_factor, @r#"
        Some(
            ScrollFactor {
                base: Some(
                    FloatOrInt(
                        1.5,
                    ),
                ),
                horizontal: Some(
                    FloatOrInt(
                        3.0,
                    ),
                ),
                vertical: None,
            },
        )
        "#);
    }

    #[test]
    fn scroll_factor_h_v_factors() {
        let sf = ScrollFactor {
            base: Some(FloatOrInt(2.0)),
            horizontal: None,
            vertical: None,
        };
        assert_debug_snapshot!(sf.h_v_factors(), @r#"
        (
            2.0,
            2.0,
        )
        "#);

        let sf = ScrollFactor {
            base: None,
            horizontal: Some(FloatOrInt(3.0)),
            vertical: Some(FloatOrInt(-1.0)),
        };
        assert_debug_snapshot!(sf.h_v_factors(), @r#"
        (
            3.0,
            -1.0,
        )
        "#);

        let sf = ScrollFactor {
            base: Some(FloatOrInt(2.0)),
            horizontal: Some(FloatOrInt(1.0)),
            vertical: None,
        };
        assert_debug_snapshot!(sf.h_v_factors(), @r"
        (
            1.0,
            2.0,
        )
        ");
    }
}

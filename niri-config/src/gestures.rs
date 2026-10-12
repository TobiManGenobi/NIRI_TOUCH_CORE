use crate::utils::MergeWith;
use crate::FloatOrInt;

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Gestures {
    pub dnd_edge_view_scroll: DndEdgeViewScroll,
    pub dnd_edge_workspace_switch: DndEdgeWorkspaceSwitch,
    pub hot_corners: HotCorners,
    /// Visualize touch points on screen as translucent dots that follow the
    /// fingers. Meant for demo videos and debugging gestures; off by
    /// default. Purely observational: it never consumes or alters touch
    /// events, so it cannot interfere with gesture recognition.
    pub show_touch_points: ShowTouchPoints,
}

#[derive(knuffel::Decode, Debug, Default, Clone, Copy, PartialEq)]
pub struct GesturesPart {
    #[knuffel(child)]
    pub dnd_edge_view_scroll: Option<DndEdgeViewScrollPart>,
    #[knuffel(child)]
    pub dnd_edge_workspace_switch: Option<DndEdgeWorkspaceSwitchPart>,
    #[knuffel(child)]
    pub hot_corners: Option<HotCorners>,
    #[knuffel(child, unwrap(argument))]
    pub show_touch_points: Option<ShowTouchPoints>,
}

impl MergeWith<GesturesPart> for Gestures {
    fn merge_with(&mut self, part: &GesturesPart) {
        merge!(
            (self, part),
            dnd_edge_view_scroll,
            dnd_edge_workspace_switch,
        );
        merge_clone!((self, part), hot_corners);
        merge_clone!((self, part), show_touch_points);
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DndEdgeViewScroll {
    pub trigger_width: f64,
    pub delay_ms: u16,
    pub max_speed: f64,
}

impl Default for DndEdgeViewScroll {
    fn default() -> Self {
        Self {
            trigger_width: 30., // Taken from GTK 4.
            delay_ms: 100,
            max_speed: 1500.,
        }
    }
}

#[derive(knuffel::Decode, Debug, Clone, Copy, PartialEq)]
pub struct DndEdgeViewScrollPart {
    #[knuffel(child, unwrap(argument))]
    pub trigger_width: Option<FloatOrInt<0, 65535>>,
    #[knuffel(child, unwrap(argument))]
    pub delay_ms: Option<u16>,
    #[knuffel(child, unwrap(argument))]
    pub max_speed: Option<FloatOrInt<0, 1_000_000>>,
}

impl MergeWith<DndEdgeViewScrollPart> for DndEdgeViewScroll {
    fn merge_with(&mut self, part: &DndEdgeViewScrollPart) {
        merge!((self, part), trigger_width, max_speed);
        merge_clone!((self, part), delay_ms);
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DndEdgeWorkspaceSwitch {
    pub trigger_height: f64,
    pub delay_ms: u16,
    pub max_speed: f64,
}

impl Default for DndEdgeWorkspaceSwitch {
    fn default() -> Self {
        Self {
            trigger_height: 50.,
            delay_ms: 100,
            max_speed: 1500.,
        }
    }
}

#[derive(knuffel::Decode, Debug, Clone, Copy, PartialEq)]
pub struct DndEdgeWorkspaceSwitchPart {
    #[knuffel(child, unwrap(argument))]
    pub trigger_height: Option<FloatOrInt<0, 65535>>,
    #[knuffel(child, unwrap(argument))]
    pub delay_ms: Option<u16>,
    #[knuffel(child, unwrap(argument))]
    pub max_speed: Option<FloatOrInt<0, 1_000_000>>,
}

impl MergeWith<DndEdgeWorkspaceSwitchPart> for DndEdgeWorkspaceSwitch {
    fn merge_with(&mut self, part: &DndEdgeWorkspaceSwitchPart) {
        merge!((self, part), trigger_height, max_speed);
        merge_clone!((self, part), delay_ms);
    }
}

#[derive(knuffel::Decode, Debug, Default, Clone, Copy, PartialEq)]
pub struct HotCorners {
    #[knuffel(child)]
    pub off: bool,
    #[knuffel(child)]
    pub top_left: bool,
    #[knuffel(child)]
    pub top_right: bool,
    #[knuffel(child)]
    pub bottom_left: bool,
    #[knuffel(child)]
    pub bottom_right: bool,
}

/// Which touch points get a dot on screen.
///
/// Purely observational. Nothing here consumes or alters touch events, so
/// turning it on cannot change what any gesture does.
///
/// ```kdl
/// gestures {
///     show-touch-points all       // every finger gets a dot
///     show-touch-points gestures  // dots only while enough fingers are
///                                // down for a gesture
/// }
/// ```
#[derive(knuffel::DecodeScalar, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum ShowTouchPoints {
    /// No dots. The default.
    #[default]
    Off,
    /// Every touch point gets a dot, including a single finger tapping.
    All,
    /// Dots appear only once enough fingers are down concurrently to drive a
    /// gesture (see `touch-arbitrate`'s `DEFAULT_MIN_GESTURE_FINGERS`), so
    /// ordinary taps and scrolling stay clean.
    Gestures,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;

    fn parse(kdl: &str) -> ShowTouchPoints {
        Config::parse_mem(&format!("gestures {{ {kdl} }}"))
            .unwrap()
            .config
            .gestures
            .show_touch_points
    }

    #[test]
    fn show_touch_points_defaults_to_off() {
        assert_eq!(parse(""), ShowTouchPoints::Off);
        assert_eq!(
            Config::default().gestures.show_touch_points,
            ShowTouchPoints::Off
        );
    }

    #[test]
    fn show_touch_points_parses_bare_and_quoted() {
        assert_eq!(parse("show-touch-points all"), ShowTouchPoints::All);
        assert_eq!(parse("show-touch-points gestures"), ShowTouchPoints::Gestures);

        // knuffel accepts the quoted spelling too; the old design used it.
        assert_eq!(parse("show-touch-points \"all\""), ShowTouchPoints::All);
        assert_eq!(
            parse("show-touch-points \"gestures\""),
            ShowTouchPoints::Gestures
        );
    }

    #[test]
    fn show_touch_points_rejects_bad_values() {
        for bad in [
            "show-touch-points",
            "show-touch-points on",
            "show-touch-points true",
            "show-touch-points 1",
            "show-touch-points All",
            "show-touch-points gesture",
        ] {
            assert!(
                Config::parse_mem(&format!("gestures {{ {bad} }}")).is_err(),
                "expected `{bad}` to be rejected"
            );
        }
    }

    /// A later `gestures` block must be able to override an earlier one,
    /// which is what `MergeWith` is for.
    #[test]
    fn show_touch_points_last_block_wins() {
        let config = Config::parse_mem(
            "
            gestures { show-touch-points all }
            gestures { show-touch-points gestures }
            ",
        )
        .unwrap()
        .config;
        assert_eq!(config.gestures.show_touch_points, ShowTouchPoints::Gestures);
    }
}

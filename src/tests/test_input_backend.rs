use smithay::backend::input::{
    AbsolutePositionEvent, Device, DeviceCapability, Event, InputBackend, InputTime, KeyState,
    KeyboardKeyEvent, Keycode, TouchCancelEvent, TouchDownEvent, TouchEvent, TouchFrameEvent,
    TouchMotionEvent, TouchSlot, TouchUpEvent, UnusedEvent,
};
use smithay::output::Output;

use crate::input::backend_ext::NiriInputDevice;
use crate::input::touch_arbitration_grab::TouchDeviceInfo;

pub struct TestInputBackend;

/// The test touch/pen digitiser.
///
/// Carries the three [`TouchDeviceInfo`] knobs so a test can describe a
/// stylus instead of a finger. It is `Eq + Hash` because niri keeps its live
/// devices in hash sets keyed by the backend's device type.
#[derive(PartialEq, Eq, Hash, Clone, Copy, Debug, Default)]
pub struct TestInputDevice {
    /// Slot id handed to [`TouchDeviceInfo::touch_device_id`]. Compared only
    /// for equality, so any stable value works.
    pub touch_device_id: u32,
    /// The digitiser is a stylus rather than a finger.
    pub is_pen: bool,
    /// The stylus tip is an eraser.
    pub is_eraser: bool,
}

pub struct TestKeyboardKeyEvent {
    pub time: InputTime,
    pub code: Keycode,
    pub state: KeyState,
    pub count: u32,
}

pub struct TestTouchDownEvent {
    pub time: InputTime,
    pub slot: u32,
    pub x: f64,
    pub y: f64,
    pub device: TestInputDevice,
}

pub struct TestTouchMotionEvent {
    pub time: InputTime,
    pub slot: u32,
    pub x: f64,
    pub y: f64,
    pub device: TestInputDevice,
}

pub struct TestTouchUpEvent {
    pub time: InputTime,
    pub slot: u32,
    pub device: TestInputDevice,
}

pub struct TestTouchCancelEvent {
    pub time: InputTime,
    /// The slot the cancel is attributed to. A cancel ends the whole session,
    /// so niri ignores this; it is here because smithay's trait requires it.
    pub slot: u32,
    pub device: TestInputDevice,
}

pub struct TestTouchFrameEvent {
    pub time: InputTime,
    pub device: TestInputDevice,
}

impl InputBackend for TestInputBackend {
    type Device = TestInputDevice;

    type KeyboardKeyEvent = TestKeyboardKeyEvent;
    type PointerAxisEvent = UnusedEvent;
    type PointerButtonEvent = UnusedEvent;
    type PointerMotionEvent = UnusedEvent;
    type PointerMotionAbsoluteEvent = UnusedEvent;

    type GestureSwipeBeginEvent = UnusedEvent;
    type GestureSwipeUpdateEvent = UnusedEvent;
    type GestureSwipeEndEvent = UnusedEvent;
    type GesturePinchBeginEvent = UnusedEvent;
    type GesturePinchUpdateEvent = UnusedEvent;
    type GesturePinchEndEvent = UnusedEvent;
    type GestureHoldBeginEvent = UnusedEvent;
    type GestureHoldEndEvent = UnusedEvent;

    type TouchDownEvent = TestTouchDownEvent;
    type TouchUpEvent = TestTouchUpEvent;
    type TouchMotionEvent = TestTouchMotionEvent;
    type TouchCancelEvent = TestTouchCancelEvent;
    type TouchFrameEvent = TestTouchFrameEvent;
    type TabletToolAxisEvent = UnusedEvent;
    type TabletToolProximityEvent = UnusedEvent;
    type TabletToolTipEvent = UnusedEvent;
    type TabletToolButtonEvent = UnusedEvent;

    type SwitchToggleEvent = UnusedEvent;

    type SpecialEvent = UnusedEvent;
}

impl Device for TestInputDevice {
    fn id(&self) -> String {
        String::from("test")
    }

    fn name(&self) -> String {
        String::from("test input device")
    }

    fn has_capability(&self, capability: DeviceCapability) -> bool {
        matches!(
            capability,
            DeviceCapability::Keyboard | DeviceCapability::Touch
        )
    }

    fn usb_id(&self) -> Option<(u32, u32)> {
        None
    }

    fn syspath(&self) -> Option<std::path::PathBuf> {
        None
    }
}

impl NiriInputDevice for TestInputDevice {
    fn output(&self, _state: &crate::niri::State) -> Option<Output> {
        None
    }
}

impl Event<TestInputBackend> for TestKeyboardKeyEvent {
    fn time(&self) -> InputTime {
        self.time
    }

    fn device(&self) -> <TestInputBackend as InputBackend>::Device {
        TestInputDevice::default()
    }
}

impl KeyboardKeyEvent<TestInputBackend> for TestKeyboardKeyEvent {
    fn key_code(&self) -> Keycode {
        self.code
    }

    fn state(&self) -> KeyState {
        self.state
    }

    fn count(&self) -> u32 {
        self.count
    }
}

impl TouchDeviceInfo for TestInputDevice {
    fn touch_device_id(&self) -> u32 {
        self.touch_device_id
    }

    fn touch_is_pen(&self) -> bool {
        self.is_pen
    }

    fn touch_is_eraser(&self) -> bool {
        self.is_eraser
    }
}

impl Event<TestInputBackend> for TestTouchDownEvent {
    fn time(&self) -> InputTime {
        self.time
    }

    fn device(&self) -> <TestInputBackend as InputBackend>::Device {
        self.device
    }
}

impl TouchEvent<TestInputBackend> for TestTouchDownEvent {
    fn slot(&self) -> TouchSlot {
        TouchSlot::from(Some(self.slot))
    }
}

impl AbsolutePositionEvent<TestInputBackend> for TestTouchDownEvent {
    fn x(&self) -> f64 {
        self.x
    }

    fn y(&self) -> f64 {
        self.y
    }

    fn x_transformed(&self, _transform: i32) -> f64 {
        self.x
    }

    fn y_transformed(&self, _transform: i32) -> f64 {
        self.y
    }
}

impl TouchDownEvent<TestInputBackend> for TestTouchDownEvent {}

impl Event<TestInputBackend> for TestTouchMotionEvent {
    fn time(&self) -> InputTime {
        self.time
    }

    fn device(&self) -> <TestInputBackend as InputBackend>::Device {
        self.device
    }
}

impl TouchEvent<TestInputBackend> for TestTouchMotionEvent {
    fn slot(&self) -> TouchSlot {
        TouchSlot::from(Some(self.slot))
    }
}

impl AbsolutePositionEvent<TestInputBackend> for TestTouchMotionEvent {
    fn x(&self) -> f64 {
        self.x
    }

    fn y(&self) -> f64 {
        self.y
    }

    fn x_transformed(&self, _transform: i32) -> f64 {
        self.x
    }

    fn y_transformed(&self, _transform: i32) -> f64 {
        self.y
    }
}

impl TouchMotionEvent<TestInputBackend> for TestTouchMotionEvent {}

impl Event<TestInputBackend> for TestTouchUpEvent {
    fn time(&self) -> InputTime {
        self.time
    }

    fn device(&self) -> <TestInputBackend as InputBackend>::Device {
        self.device
    }
}

impl TouchEvent<TestInputBackend> for TestTouchUpEvent {
    fn slot(&self) -> TouchSlot {
        TouchSlot::from(Some(self.slot))
    }
}

impl TouchUpEvent<TestInputBackend> for TestTouchUpEvent {}

impl Event<TestInputBackend> for TestTouchCancelEvent {
    fn time(&self) -> InputTime {
        self.time
    }

    fn device(&self) -> <TestInputBackend as InputBackend>::Device {
        self.device
    }
}

impl TouchEvent<TestInputBackend> for TestTouchCancelEvent {
    fn slot(&self) -> TouchSlot {
        TouchSlot::from(Some(self.slot))
    }
}

impl TouchCancelEvent<TestInputBackend> for TestTouchCancelEvent {}

impl Event<TestInputBackend> for TestTouchFrameEvent {
    fn time(&self) -> InputTime {
        self.time
    }

    fn device(&self) -> <TestInputBackend as InputBackend>::Device {
        self.device
    }
}

impl TouchFrameEvent<TestInputBackend> for TestTouchFrameEvent {}

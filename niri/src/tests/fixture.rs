use std::os::fd::AsFd as _;
use std::os::unix::net::UnixStream;
use std::sync::atomic::Ordering;
use std::time::Duration;

use calloop::generic::Generic;
use calloop::{EventLoop, Interest, LoopHandle, Mode, PostAction};
use niri_config::input::ModKey;
use niri_config::Config;
use smithay::backend::input::{InputEvent, InputTime};
use smithay::input::keyboard::ModifiersState;
use smithay::output::Output;

use super::client::{Client, ClientId};
use super::server::Server;
use super::test_input_backend::{
    TestInputBackend, TestInputDevice, TestTouchCancelEvent, TestTouchDownEvent,
    TestTouchFrameEvent, TestTouchMotionEvent, TestTouchUpEvent,
};
use crate::niri::{NewClient, Niri};

pub struct Fixture {
    pub event_loop: EventLoop<'static, State>,
    /// The digitiser every touch event is reported from. Mutable through
    /// [`Fixture::set_pen`] and friends so one test can describe a stylus
    /// and another a finger.
    pub touch_device: TestInputDevice,
    pub handle: LoopHandle<'static, State>,
    pub state: State,
}

pub struct State {
    pub server: Server,
    pub clients: Vec<Client>,
}

impl Fixture {
    pub fn new() -> Self {
        Self::with_config(Config::default())
    }

    pub fn with_config(config: Config) -> Self {
        let event_loop = EventLoop::try_new().unwrap();
        let handle = event_loop.handle();

        let server = Server::new(config);
        let fd = server.event_loop.as_fd().try_clone_to_owned().unwrap();
        let source = Generic::new(fd, Interest::READ, Mode::Level);
        handle
            .insert_source(source, |_, _, state: &mut State| {
                state.server.dispatch();
                Ok(PostAction::Continue)
            })
            .unwrap();

        let state = State {
            server,
            clients: Vec::new(),
        };

        // The touch ledgers are `thread_local!` and outlive any one fixture,
        // so a fresh compositor would otherwise inherit the previous test's
        // contacts and open-client count. Clear them here rather than making
        // every touch test remember to.
        crate::input::touch_arbitration_grab::reset_persistent();
        // The recognised-gesture seam is cleared on ENTRY to a sequence, not by
        // `reset_persistent`, because that runs at the END of a sequence — too
        // late to be readable. A fresh fixture has begun no sequence and
        // recognised nothing, so starting one is the semantically right reset
        // and is what makes the contamination guard below meaningful.
        crate::input::touch_arbitration_grab::begin_sequence();

        // Contamination guard. These ledgers are thread_local and survive
        // between `cargo test` cases on the same worker thread, so a fixture
        // that inherited state would make a later test assert against an
        // earlier test's value — a failure that reads like a compositor bug
        // but is a harness bug. Fail here, loudly, rather than three tests
        // later.
        debug_assert!(
            crate::input::touch_arbitration_grab::live_contacts().is_empty(),
            "fixture inherited a dirty live-contact ledger"
        );
        debug_assert_eq!(
            crate::input::touch_arbitration_grab::open_client_touches(),
            0,
            "fixture inherited a dirty ownership ledger"
        );
        // The recognised-gesture seam is cleared by the same
        // `reset_persistent()` call above; assert it here for the same reason.
        debug_assert!(
            crate::input::touch_arbitration_grab::last_recognised_gesture().is_none(),
            "fixture inherited a stale recognised gesture from a previous test"
        );

        Self {
            event_loop,
            handle,
            touch_device: TestInputDevice::default(),
            state,
        }
    }

    pub fn dispatch(&mut self) {
        self.event_loop
            .dispatch(Duration::ZERO, &mut self.state)
            .unwrap();
    }

    pub fn niri_state(&mut self) -> &mut crate::niri::State {
        &mut self.state.server.state
    }

    pub fn niri(&mut self) -> &mut Niri {
        &mut self.niri_state().niri
    }

    pub fn niri_output(&self, n: u8) -> Output {
        let niri = &self.state.server.state.niri;
        let idx = usize::from(n - 1);
        let output = niri.global_space.outputs().nth(idx).unwrap();
        output.clone()
    }

    pub fn niri_focus_output(&mut self, n: u8) {
        let niri = &mut self.state.server.state.niri;
        let idx = usize::from(n - 1);
        let output = niri.global_space.outputs().nth(idx).unwrap();
        niri.layout.focus_output(output);
    }

    pub fn niri_complete_animations(&mut self) {
        let niri = self.niri();
        niri.clock.set_complete_instantly(true);
        niri.advance_animations();
        niri.clock.set_complete_instantly(false);
    }

    pub fn add_output(&mut self, n: u8, size: (u16, u16)) {
        let state = self.niri_state();
        let niri = &mut state.niri;
        state.backend.headless().add_output(niri, n, size);
    }

    pub fn add_client(&mut self) -> ClientId {
        let (sock1, sock2) = UnixStream::pair().unwrap();
        self.niri().insert_client(NewClient {
            client: sock1,
            restricted: false,
            credentials_unknown: false,
        });

        let client = Client::new(sock2);
        let id = client.id;

        let fd = client.event_loop.as_fd().try_clone_to_owned().unwrap();
        let source = Generic::new(fd, Interest::READ, Mode::Level);
        self.handle
            .insert_source(source, move |_, _, state: &mut State| {
                state.client(id).dispatch();
                Ok(PostAction::Continue)
            })
            .unwrap();

        self.state.clients.push(client);
        self.roundtrip(id);
        id
    }

    pub fn client(&mut self, id: ClientId) -> &mut Client {
        self.state.client(id)
    }

    pub fn roundtrip(&mut self, id: ClientId) {
        let client = self.state.client(id);
        let data = client.send_sync();
        while !data.done.load(Ordering::Relaxed) {
            self.dispatch();
        }
    }

    /// Roundtrip twice in a row.
    ///
    /// For some reason, when running tests on many threads at once, a single roundtrip is
    /// sometimes not sufficient to get the configure events to the client.
    ///
    /// I suspect that this is because these configure events are sent from the niri loop callback,
    /// so they arrive after the sync done event and don't get processed in that client dispatch
    /// cycle. I'm not sure why this would be dependent on multithreading. But if this is indeed
    /// the issue, then a double roundtrip fixes it.
    pub fn double_roundtrip(&mut self, id: ClientId) {
        self.roundtrip(id);
        self.roundtrip(id);
    }

    /// Register the test device with niri so the seat gains a touch handler.
    ///
    /// Without this the seat has no `wl_touch` and every touch event is
    /// dropped at the top of the handler.
    pub fn add_touch_device(&mut self) {
        self.process_input(InputEvent::DeviceAdded {
            device: self.touch_device,
        });
    }

    /// Send one input event through the genuine `process_input_event` path.
    fn process_input(&mut self, event: InputEvent<TestInputBackend>) {
        self.state.server.state.process_input_event(event);
    }

    /// A contact landing at `(x, y)` on `slot`.
    ///
    /// `time` is explicit and in microseconds: these tests must not depend on
    /// the wall clock.
    pub fn touch_down(&mut self, slot: u32, x: f64, y: f64, time: Duration) {
        self.process_input(InputEvent::TouchDown {
            event: TestTouchDownEvent {
                time: InputTime::from_micros(time.as_micros() as u64),
                slot,
                x,
                y,
                device: self.touch_device,
            },
        });
    }

    /// The contact on `slot` moving to `(x, y)`.
    pub fn touch_motion(&mut self, slot: u32, x: f64, y: f64, time: Duration) {
        self.process_input(InputEvent::TouchMotion {
            event: TestTouchMotionEvent {
                time: InputTime::from_micros(time.as_micros() as u64),
                slot,
                x,
                y,
                device: self.touch_device,
            },
        });
    }

    /// The contact on `slot` lifting.
    pub fn touch_up(&mut self, slot: u32, time: Duration) {
        self.process_input(InputEvent::TouchUp {
            event: TestTouchUpEvent {
                time: InputTime::from_micros(time.as_micros() as u64),
                slot,
                device: self.touch_device,
            },
        });
    }

    /// End the frame, as a real digitiser does after a batch of contacts.
    pub fn touch_frame(&mut self, time: Duration) {
        self.process_input(InputEvent::TouchFrame {
            event: TestTouchFrameEvent {
                time: InputTime::from_micros(time.as_micros() as u64),
                device: self.touch_device,
            },
        });
    }

    /// Cancel every contact at once, naming none of them.
    pub fn touch_cancel(&mut self, time: Duration) {
        self.process_input(InputEvent::TouchCancel {
            event: TestTouchCancelEvent {
                time: InputTime::from_micros(time.as_micros() as u64),
                slot: 0,
                device: self.touch_device,
            },
        });
    }

    /// Describe the digitiser as a stylus rather than a finger.
    ///
    /// Applies to every subsequent contact, which is what a real digitiser
    /// does: the tool is a property of the device, not of one contact.
    pub fn set_pen(&mut self, is_pen: bool) {
        self.touch_device = TestInputDevice {
            is_pen,
            ..self.touch_device
        };
    }

    /// Make the next contacts report an eraser tip.
    pub fn set_eraser(&mut self, is_eraser: bool) {
        self.touch_device = TestInputDevice {
            is_eraser,
            ..self.touch_device
        };
    }

    /// The slot id handed to the touch arbitrator for this device.
    pub fn set_touch_device_id(&mut self, id: u32) {
        self.touch_device = TestInputDevice {
            touch_device_id: id,
            ..self.touch_device
        };
    }

    /// Hold Mod down, exactly as a user pressing the Mod key would.
    ///
    /// Mod lives in the seat's keyboard, so the only honest way to set it is
    /// to press the key: the touch arbitrator reads the same modifier state
    /// the bind table does.
    pub fn mod_down(&mut self) {
        self.set_mod(true);
    }

    /// Release Mod.
    pub fn mod_up(&mut self) {
        self.set_mod(false);
    }

    /// Hold or release Mod by setting the seat's modifier state directly.
    ///
    /// Deliberately NOT done by pressing a keycode: that needs the Mod key to
    /// be present in the active xkb keymap, and a headless test keymap has no
    /// `Super_L`. These tests are about what Mod *means* to the touch
    /// arbitrator, not about xkb keymap contents, so they must not depend on
    /// which keys a layout happens to define. `modifiers_from_state` reads
    /// exactly this state, so setting it asserts the real contract.
    fn set_mod(&mut self, pressed: bool) {
        let state = self.niri_state();
        let mod_key = state.backend.mod_key(&state.niri.config.borrow());

        let mut mods = ModifiersState::default();
        match mod_key {
            ModKey::Ctrl => mods.ctrl = pressed,
            ModKey::Shift => mods.shift = pressed,
            ModKey::Alt => mods.alt = pressed,
            ModKey::Super => mods.logo = pressed,
            ModKey::IsoLevel3Shift => mods.iso_level3_shift = pressed,
            ModKey::IsoLevel5Shift => mods.iso_level5_shift = pressed,
        }

        state
            .niri
            .seat
            .get_keyboard()
            .unwrap()
            .set_modifier_state(mods);
    }

    /// How many `test-action`s niri has run.
    pub fn test_action_count(&mut self) -> usize {
        self.niri().test_action_count
    }
}

impl State {
    pub fn client(&mut self, id: ClientId) -> &mut Client {
        self.clients.iter_mut().find(|c| c.id == id).unwrap()
    }
}

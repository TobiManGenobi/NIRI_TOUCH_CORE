use std::cell::RefCell;
use std::collections::{HashSet, VecDeque};
use std::ffi::OsStr;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;
use std::{env, io, process};

use anyhow::Context;
use async_channel::{Receiver, Sender, TrySendError};
use calloop::futures::Scheduler;
use calloop::io::Async;
use directories::BaseDirs;
use futures_util::io::{AsyncReadExt, BufReader};
use futures_util::{select_biased, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, FutureExt as _};
use niri_config::OutputName;
use niri_ipc::state::{EventStreamState, EventStreamStatePart as _};
use niri_ipc::{
    Action, Event, KeyboardLayouts, OutputConfigChanged, Overview, Reply, Request, Response,
    Timestamp, TouchOwnership, TouchTrace, TouchTraceEntry, WindowLayout, Workspace,
    TOUCH_TRACE_CAPACITY,
};
use smithay::desktop::layer_map_for_output;
use smithay::input::pointer::{
    CursorIcon, CursorImageStatus, Focus, GrabStartData as PointerGrabStartData,
};
use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::{Interest, LoopHandle, Mode, PostAction};
use smithay::reexports::rustix::fs::unlink;
use smithay::utils::SERIAL_COUNTER;
use smithay::wayland::shell::wlr_layer::{KeyboardInteractivity, Layer};

use crate::backend::IpcOutputMap;
use crate::handlers::image_copy_capture;
use crate::input::pick_window_grab::PickWindowGrab;
use crate::layout::workspace::WorkspaceId;
use crate::niri::State;
use crate::utils::{version, with_toplevel_role};
use crate::window::Mapped;

// If an event stream client fails to read events fast enough that we accumulate more than this
// number in our buffer, we drop that event stream client.
const EVENT_STREAM_BUFFER_SIZE: usize = 64;

/// Bounded ring buffer of the most recent touch-trace events.
///
/// The touch events are push-only, so a debugging client that attaches after
/// the bug has already happened has nothing to look at. This keeps a short
/// recent history so it can be read back with [`Request::TouchTrace`], the
/// same shape as Android's `InputDispatcher::mRecentQueue`.
///
/// Deliberately not a full replay log: the last few hundred entries are what
/// explain a gesture that misbehaved, and an unbounded buffer on a path that
/// runs on every finger-down would be an unbounded memory cost for history
/// nobody will ever read.
///
/// The hot path is a `VecDeque` with reserved capacity: past the first
/// [`TOUCH_TRACE_CAPACITY`] pushes it reuses the slot freed by the `pop_front`
/// and allocates nothing.
struct TouchTraceRing {
    entries: VecDeque<TouchTraceEntry>,
    /// Sequence number to hand to the next entry. Never 0, never reused.
    next_sequence: u64,
}

impl TouchTraceRing {
    fn new() -> Self {
        Self {
            entries: VecDeque::with_capacity(TOUCH_TRACE_CAPACITY),
            next_sequence: 1,
        }
    }

    /// Append one event, dropping the oldest entry if the buffer is full.
    fn push(&mut self, event: Event) {
        let sequence = self.next_sequence;
        self.next_sequence += 1;

        if self.entries.len() == TOUCH_TRACE_CAPACITY {
            // Reuse the slot the popped entry owned rather than allocating.
            self.entries.pop_front();
        }

        let timestamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(Timestamp { secs: 0, nanos: 0 }, Timestamp::from);
        self.entries.push_back(TouchTraceEntry {
            sequence,
            timestamp,
            event,
        });
    }

    /// Entries strictly newer than `since`, oldest first.
    fn since(&self, since: u64) -> TouchTrace {
        TouchTrace {
            entries: self
                .entries
                .iter()
                .filter(|entry| entry.sequence > since)
                .cloned()
                .collect(),
            next_sequence: self.next_sequence,
        }
    }
}

/// Record one touch event in the ring, and decide whether it also goes to the
/// live event stream.
///
/// The ring is written first and unconditionally; `broadcast` only controls the
/// stream. Splitting it this way is what keeps
/// [`Request::TouchTrace`] useful in a session that never set
/// `NIRI_IPC_TOUCH_TRACE=1`, while the stream stays opt-in.
fn record_touch_event(ring: &mut TouchTraceRing, event: Event, broadcast: bool) -> Option<Event> {
    ring.push(event.clone());
    broadcast.then_some(event)
}

pub struct IpcServer {
    /// Path to the IPC socket.
    ///
    /// This is `None` when creating `IpcServer` without a socket.
    pub socket_path: Option<PathBuf>,
    event_streams: Rc<RefCell<Vec<EventStreamSender>>>,
    event_stream_state: Rc<RefCell<EventStreamState>>,
    /// Bounded history of the touch events, for late-attaching clients.
    ///
    /// Shared with the request handlers so that [`Request::TouchTrace`] can
    /// read it; `Rc<RefCell<…>>` like the other IPC server state, and touched
    /// only from the main loop thread, so the borrow can never deadlock
    /// against the event-stream emit path (which is also main-loop only).
    touch_trace: Rc<RefCell<TouchTraceRing>>,
    /// Whether to broadcast the touch-arbitration events on the event stream.
    ///
    /// Unlike the gesture events, which are per-bind opt-in, arbitration
    /// decisions happen on every finger-down and there is nothing per-bind to
    /// attach them to: a gesture that never gets recognised has no bind to
    /// ask for it. Since the *stream* exists for debugging and can flood a
    /// real subscriber with one line per finger-down, it is gated on an
    /// environment variable read once at startup.
    ///
    /// This gates the live stream only. The [`Self::touch_trace`] ring buffer
    /// is populated unconditionally, so [`Request::TouchTrace`] works in any
    /// session, including one that never set the variable.
    pub trace_touch_arbitration: bool,
}

struct ClientCtx {
    event_loop: LoopHandle<'static, State>,
    scheduler: Scheduler<()>,
    ipc_outputs: Arc<Mutex<IpcOutputMap>>,
    event_streams: Rc<RefCell<Vec<EventStreamSender>>>,
    event_stream_state: Rc<RefCell<EventStreamState>>,
    touch_trace: Rc<RefCell<TouchTraceRing>>,
}

struct EventStreamClient {
    events: Receiver<Event>,
    disconnect: Receiver<()>,
    write: Box<dyn AsyncWrite + Unpin>,
}

struct EventStreamSender {
    events: Sender<Event>,
    disconnect: Sender<()>,
}

impl IpcServer {
    pub fn start(
        event_loop: &LoopHandle<'static, State>,
        wayland_socket_name: Option<&OsStr>,
    ) -> anyhow::Result<Self> {
        let _span = tracy_client::span!("Ipc::start");

        let socket_path = if let Some(wayland_socket_name) = wayland_socket_name {
            let wayland_socket_name = wayland_socket_name.to_string_lossy();
            let socket_name = format!("niri.{wayland_socket_name}.{}.sock", process::id());
            let mut socket_path = socket_dir();
            socket_path.push(socket_name);

            let listener = UnixListener::bind(&socket_path).context("error binding socket")?;
            listener
                .set_nonblocking(true)
                .context("error setting socket to non-blocking")?;

            let source = Generic::new(listener, Interest::READ, Mode::Level);
            event_loop
                .insert_source(source, |_, socket, state| {
                    match socket.accept() {
                        Ok((stream, _)) => on_new_ipc_client(state, stream),
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => (),
                        Err(e) => return Err(e),
                    }

                    Ok(PostAction::Continue)
                })
                .unwrap();

            Some(socket_path)
        } else {
            None
        };

        Ok(Self {
            socket_path,
            event_streams: Rc::new(RefCell::new(Vec::new())),
            event_stream_state: Rc::new(RefCell::new(EventStreamState::default())),
            touch_trace: Rc::new(RefCell::new(TouchTraceRing::new())),
            trace_touch_arbitration: env::var_os("NIRI_IPC_TOUCH_TRACE").is_some_and(|x| x == "1"),
        })
    }

    fn send_event(&self, event: Event) {
        let mut streams = self.event_streams.borrow_mut();
        let mut to_remove = Vec::new();
        for (idx, stream) in streams.iter_mut().enumerate() {
            match stream.events.try_send(event.clone()) {
                Ok(()) => (),
                Err(TrySendError::Closed(_)) => to_remove.push(idx),
                Err(TrySendError::Full(_)) => {
                    warn!(
                        "disconnecting IPC event stream client \
                         because it is reading events too slowly"
                    );
                    to_remove.push(idx);
                }
            }
        }

        for idx in to_remove.into_iter().rev() {
            let stream = streams.swap_remove(idx);
            let _ = stream.disconnect.send_blocking(());
        }
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        if let Some(socket_path) = &self.socket_path {
            let _ = unlink(socket_path);
        }
    }
}

fn socket_dir() -> PathBuf {
    BaseDirs::new()
        .as_ref()
        .and_then(|x| x.runtime_dir())
        .map(|x| x.to_owned())
        .unwrap_or_else(env::temp_dir)
}

fn on_new_ipc_client(state: &mut State, stream: UnixStream) {
    let _span = tracy_client::span!("on_new_ipc_client");
    trace!("new IPC client connected");

    let stream = match state.niri.event_loop.adapt_io(stream) {
        Ok(stream) => stream,
        Err(err) => {
            warn!("error making IPC stream async: {err:?}");
            return;
        }
    };

    let ipc_server = state.niri.ipc_server.as_ref().unwrap();

    let ctx = ClientCtx {
        event_loop: state.niri.event_loop.clone(),
        scheduler: state.niri.scheduler.clone(),
        ipc_outputs: state.backend.ipc_outputs(),
        event_streams: ipc_server.event_streams.clone(),
        event_stream_state: ipc_server.event_stream_state.clone(),
        touch_trace: ipc_server.touch_trace.clone(),
    };

    let future = async move {
        if let Err(err) = handle_client(ctx, stream).await {
            warn!("error handling IPC client: {err:?}");
        }
    };
    if let Err(err) = state.niri.scheduler.schedule(future) {
        warn!("error scheduling IPC stream future: {err:?}");
    }
}

async fn handle_client(ctx: ClientCtx, stream: Async<'static, UnixStream>) -> anyhow::Result<()> {
    let (read, mut write) = stream.split();
    let mut read = BufReader::new(read);

    loop {
        // Don't keep buf around to avoid clients wasting RAM by filling it with bogus data.
        let mut buf = Vec::new();
        let res = read.read_until(b'\n', &mut buf).await;
        match res {
            Ok(0) => return Ok(()),
            Ok(_) => (),
            // Normal client disconnection.
            Err(err) if err.kind() == io::ErrorKind::BrokenPipe => return Ok(()),
            Err(err) => {
                return Err(err).context("error reading request");
            }
        }

        let request = serde_json::from_slice(&buf)
            .context("error parsing request")
            .map_err(|err| err.to_string());
        let requested_error = matches!(request, Ok(Request::ReturnError));
        let requested_event_stream = matches!(request, Ok(Request::EventStream));

        let reply = match request {
            Ok(request) => process(&ctx, request).await,
            Err(err) => Err(err),
        };

        if let Err(err) = &reply {
            if !requested_error {
                warn!("error processing IPC request: {err:?}");
            }
        }

        buf.clear();
        serde_json::to_writer(&mut buf, &reply).context("error formatting reply")?;
        buf.push(b'\n');
        write.write_all(&buf).await.context("error writing reply")?;

        if requested_event_stream {
            let (events_tx, events_rx) = async_channel::bounded(EVENT_STREAM_BUFFER_SIZE);
            let (disconnect_tx, disconnect_rx) = async_channel::bounded(1);

            // Spawn a task for the client.
            let client = EventStreamClient {
                events: events_rx,
                disconnect: disconnect_rx,
                write: Box::new(write) as _,
            };
            let future = async move {
                if let Err(err) = handle_event_stream_client(client).await {
                    warn!("error handling IPC event stream client: {err:?}");
                }
            };
            if let Err(err) = ctx.scheduler.schedule(future) {
                warn!("error scheduling IPC event stream future: {err:?}");
            }

            // Send the initial state.
            {
                let state = ctx.event_stream_state.borrow();
                for event in state.replicate() {
                    events_tx
                        .try_send(event)
                        .expect("initial event burst had more events than buffer size");
                }
            }

            // Add it to the list.
            {
                let mut streams = ctx.event_streams.borrow_mut();
                let sender = EventStreamSender {
                    events: events_tx,
                    disconnect: disconnect_tx,
                };
                streams.push(sender);
            }

            return Ok(());
        }
    }
}

async fn process(ctx: &ClientCtx, request: Request) -> Reply {
    let response = match request {
        Request::ReturnError => return Err(String::from("example compositor error")),
        Request::Version => Response::Version(version()),
        Request::Outputs => {
            let ipc_outputs = ctx.ipc_outputs.lock().unwrap().clone();
            let outputs = ipc_outputs.values().cloned().map(|o| (o.name.clone(), o));
            Response::Outputs(outputs.collect())
        }
        Request::Workspaces => {
            let state = ctx.event_stream_state.borrow();
            let workspaces = state.workspaces.workspaces.values().cloned().collect();
            Response::Workspaces(workspaces)
        }
        Request::Windows => {
            let state = ctx.event_stream_state.borrow();
            let windows = state.windows.windows.values().cloned().collect();
            Response::Windows(windows)
        }
        Request::Layers => {
            let (tx, rx) = async_channel::bounded(1);
            ctx.event_loop.insert_idle(move |state| {
                let mut layers = Vec::new();
                for output in state.niri.global_space.outputs() {
                    let name = output.name();
                    for surface in layer_map_for_output(output).layers() {
                        let layer = match surface.layer() {
                            Layer::Background => niri_ipc::Layer::Background,
                            Layer::Bottom => niri_ipc::Layer::Bottom,
                            Layer::Top => niri_ipc::Layer::Top,
                            Layer::Overlay => niri_ipc::Layer::Overlay,
                        };
                        let keyboard_interactivity =
                            match surface.cached_state().keyboard_interactivity {
                                KeyboardInteractivity::None => {
                                    niri_ipc::LayerSurfaceKeyboardInteractivity::None
                                }
                                KeyboardInteractivity::Exclusive => {
                                    niri_ipc::LayerSurfaceKeyboardInteractivity::Exclusive
                                }
                                KeyboardInteractivity::OnDemand => {
                                    niri_ipc::LayerSurfaceKeyboardInteractivity::OnDemand
                                }
                            };

                        layers.push(niri_ipc::LayerSurface {
                            namespace: surface.namespace().to_owned(),
                            output: name.clone(),
                            layer,
                            keyboard_interactivity,
                        });
                    }
                }

                let _ = tx.send_blocking(layers);
            });
            let result = rx.recv().await;
            let layers = result.map_err(|_| String::from("error getting layers info"))?;
            Response::Layers(layers)
        }
        Request::KeyboardLayouts => {
            let state = ctx.event_stream_state.borrow();
            let layout = state.keyboard_layouts.keyboard_layouts.clone();
            let layout = layout.expect("keyboard layouts should be set at startup");
            Response::KeyboardLayouts(layout)
        }
        Request::FocusedWindow => {
            let state = ctx.event_stream_state.borrow();
            let windows = &state.windows.windows;
            let window = windows.values().find(|win| win.is_focused).cloned();
            Response::FocusedWindow(window)
        }
        Request::PickWindow => {
            let (tx, rx) = async_channel::bounded(1);
            ctx.event_loop.insert_idle(move |state| {
                let pointer = state.niri.seat.get_pointer().unwrap();
                let start_data = PointerGrabStartData {
                    focus: None,
                    button: 0,
                    location: pointer.current_location(),
                };
                let grab = PickWindowGrab::new(start_data);
                // The `WindowPickGrab` ungrab handler will cancel the previous ongoing pick, if
                // any.
                pointer.set_grab(state, grab, SERIAL_COUNTER.next_serial(), Focus::Clear);
                state.niri.pick_window = Some(tx);
                state
                    .niri
                    .cursor_manager
                    .set_cursor_image(CursorImageStatus::Named(CursorIcon::Crosshair));
                // Redraw to update the cursor.
                state.niri.queue_redraw_all();
            });
            let result = rx.recv().await;
            let id = result.map_err(|_| String::from("error getting picked window info"))?;
            let window = id.and_then(|id| {
                let state = ctx.event_stream_state.borrow();
                state.windows.windows.get(&id.get()).cloned()
            });
            Response::PickedWindow(window)
        }
        Request::PickColor => {
            let (tx, rx) = async_channel::bounded(1);
            ctx.event_loop.insert_idle(move |state| {
                state.handle_pick_color(tx);
            });
            let result = rx.recv().await;
            let color = result.map_err(|_| String::from("error getting picked color"))?;
            Response::PickedColor(color)
        }
        Request::Action(action) => {
            validate_action(&action)?;

            let (tx, rx) = async_channel::bounded(1);

            let action = niri_config::Action::from(action);
            ctx.event_loop.insert_idle(move |state| {
                // Make sure some logic like workspace clean-up has a chance to run before doing
                // actions.
                state.niri.advance_animations();
                state.do_action(action, false);
                let _ = tx.send_blocking(());
            });

            // Wait until the action has been processed before returning. This is important for a
            // few actions, for instance for DoScreenTransition this wait ensures that the screen
            // contents were sampled into the texture.
            let _ = rx.recv().await;
            Response::Handled
        }
        Request::Output { output, action } => {
            action.validate()?;

            let ipc_outputs = ctx.ipc_outputs.lock().unwrap();
            let found = ipc_outputs
                .values()
                .any(|o| OutputName::from_ipc_output(o).matches(&output));
            let response = if found {
                OutputConfigChanged::Applied
            } else {
                OutputConfigChanged::OutputWasMissing
            };
            drop(ipc_outputs);

            ctx.event_loop.insert_idle(move |state| {
                state.apply_transient_output_config(&output, action);
            });

            Response::OutputConfigChanged(response)
        }
        Request::FocusedOutput => {
            let (tx, rx) = async_channel::bounded(1);
            ctx.event_loop.insert_idle(move |state| {
                let active_output = state
                    .niri
                    .layout
                    .active_output()
                    .map(|output| output.name());

                let output = active_output.and_then(|active_output| {
                    state
                        .backend
                        .ipc_outputs()
                        .lock()
                        .unwrap()
                        .values()
                        .find(|o| o.name == active_output)
                        .cloned()
                });

                let _ = tx.send_blocking(output);
            });
            let result = rx.recv().await;
            let output = result.map_err(|_| String::from("error getting active output info"))?;
            Response::FocusedOutput(output)
        }
        Request::EventStream => Response::Handled,
        Request::OverviewState => {
            let state = ctx.event_stream_state.borrow();
            let is_open = state.overview.is_open;
            Response::OverviewState(Overview { is_open })
        }
        Request::Casts => {
            let state = ctx.event_stream_state.borrow();
            let casts = state.casts.casts.values().cloned().collect();
            Response::Casts(casts)
        }
        Request::TouchTrace { since } => {
            let ring = ctx.touch_trace.borrow();
            Response::TouchTrace(ring.since(since))
        }
    };

    Ok(response)
}

fn validate_action(action: &Action) -> Result<(), String> {
    if let Action::Screenshot { path, .. }
    | Action::ScreenshotScreen { path, .. }
    | Action::ScreenshotWindow { path, .. }
    | Action::LoadConfigFile { path } = action
    {
        if let Some(path) = path {
            // Relative paths are resolved against the niri compositor's working directory, which
            // is almost certainly not what you want.
            if !Path::new(path).is_absolute() {
                return Err(format!("path must be absolute: {path}"));
            }
        }
    }

    if let Action::LoadConfigFile { path: Some(path) } = action {
        let p = Path::new(path);
        if !p.is_file() {
            return Err(format!("path does not point to a file: {path}"));
        }
    }

    Ok(())
}

async fn handle_event_stream_client(client: EventStreamClient) -> anyhow::Result<()> {
    let EventStreamClient {
        events,
        disconnect,
        mut write,
    } = client;

    while let Ok(event) = events.recv().await {
        let mut buf = serde_json::to_vec(&event).context("error formatting event")?;
        buf.push(b'\n');

        let res = select_biased! {
            _ = disconnect.recv().fuse() => return Ok(()),
            res = write.write_all(&buf).fuse() => res,
        };

        match res {
            Ok(()) => (),
            // Normal client disconnection.
            Err(err) if err.kind() == io::ErrorKind::BrokenPipe => return Ok(()),
            res @ Err(_) => res.context("error writing event")?,
        }
    }

    Ok(())
}

fn make_ipc_window(
    mapped: &Mapped,
    workspace_id: Option<WorkspaceId>,
    layout: WindowLayout,
) -> niri_ipc::Window {
    with_toplevel_role(mapped.toplevel(), |role| niri_ipc::Window {
        id: mapped.id().get(),
        title: role.title.clone(),
        app_id: role.app_id.clone(),
        pid: mapped.credentials().map(|c| c.pid),
        workspace_id: workspace_id.map(|id| id.get()),
        is_focused: mapped.is_focused(),
        is_floating: mapped.is_floating(),
        is_urgent: mapped.is_urgent(),
        layout,
        focus_timestamp: mapped.get_focus_timestamp().map(Timestamp::from),
    })
}

impl State {
    pub fn ipc_keyboard_layouts_changed(&mut self) {
        let keyboard = self.niri.seat.get_keyboard().unwrap();
        let keyboard_layouts = keyboard.with_xkb_state(self, |context| {
            let xkb = context.xkb().lock().unwrap();
            let layouts = xkb.layouts();
            KeyboardLayouts {
                names: layouts
                    .map(|layout| xkb.layout_name(layout).to_owned())
                    .collect(),
                current_idx: xkb.active_layout().0 as u8,
            }
        });

        let Some(server) = &self.niri.ipc_server else {
            return;
        };

        let mut state = server.event_stream_state.borrow_mut();
        let state = &mut state.keyboard_layouts;

        let event = Event::KeyboardLayoutsChanged { keyboard_layouts };
        state.apply(event.clone());
        server.send_event(event);
    }

    pub fn ipc_refresh_keyboard_layout_index(&mut self) {
        let keyboard = self.niri.seat.get_keyboard().unwrap();
        let idx = keyboard.with_xkb_state(self, |context| {
            let xkb = context.xkb().lock().unwrap();
            xkb.active_layout().0 as u8
        });

        let Some(server) = &self.niri.ipc_server else {
            return;
        };

        let mut state = server.event_stream_state.borrow_mut();
        let state = &mut state.keyboard_layouts;

        if state.keyboard_layouts.as_ref().unwrap().current_idx == idx {
            return;
        }

        let event = Event::KeyboardLayoutSwitched { idx };
        state.apply(event.clone());
        server.send_event(event);
    }

    pub fn ipc_refresh_layout(&mut self) {
        self.ipc_refresh_workspaces();
        self.ipc_refresh_windows();
        self.ipc_refresh_overview();
    }

    fn ipc_refresh_workspaces(&mut self) {
        let Some(server) = &self.niri.ipc_server else {
            return;
        };

        let _span = tracy_client::span!("State::ipc_refresh_workspaces");

        let mut state = server.event_stream_state.borrow_mut();
        let state = &mut state.workspaces;

        let mut events = Vec::new();
        let layout = &self.niri.layout;
        let focused_ws_id = layout.active_workspace().map(|ws| ws.id().get());

        // Check for workspace changes.
        let mut seen = HashSet::new();
        let mut need_workspaces_changed = false;
        for (mon, ws_idx, ws) in layout.workspaces() {
            let id = ws.id().get();
            seen.insert(id);

            let Some(ipc_ws) = state.workspaces.get(&id) else {
                // A new workspace was added.
                need_workspaces_changed = true;
                break;
            };

            // Check for any changes that we can't signal as individual events.
            let output_name = mon.map(|mon| mon.output_name());
            if ipc_ws.idx != u8::try_from(ws_idx + 1).unwrap_or(u8::MAX)
                || ipc_ws.name.as_ref() != ws.name()
                || ipc_ws.output.as_ref() != output_name
            {
                need_workspaces_changed = true;
                break;
            }

            let active_window_id = ws.active_window().map(|win| win.id().get());
            if ipc_ws.active_window_id != active_window_id {
                events.push(Event::WorkspaceActiveWindowChanged {
                    workspace_id: id,
                    active_window_id,
                });
            }

            // Check if this workspace urgent state changed.
            let urgent = ws.is_urgent();
            if urgent != ipc_ws.is_urgent {
                events.push(Event::WorkspaceUrgencyChanged { id, urgent });
            }

            // Check if this workspace became focused.
            let is_focused = Some(id) == focused_ws_id;
            if is_focused && !ipc_ws.is_focused {
                events.push(Event::WorkspaceActivated { id, focused: true });
                continue;
            }

            // Check if this workspace became active.
            let is_active = mon.is_some_and(|mon| mon.active_workspace_idx() == ws_idx);
            if is_active && !ipc_ws.is_active {
                events.push(Event::WorkspaceActivated { id, focused: false });
            }
        }

        // Check if any workspaces were removed.
        if !need_workspaces_changed && state.workspaces.keys().any(|id| !seen.contains(id)) {
            need_workspaces_changed = true;
        }

        if need_workspaces_changed {
            events.clear();

            let workspaces = layout
                .workspaces()
                .map(|(mon, ws_idx, ws)| {
                    let id = ws.id().get();
                    Workspace {
                        id,
                        idx: u8::try_from(ws_idx + 1).unwrap_or(u8::MAX),
                        name: ws.name().cloned(),
                        output: mon.map(|mon| mon.output_name().clone()),
                        is_urgent: ws.is_urgent(),
                        is_active: mon.is_some_and(|mon| mon.active_workspace_idx() == ws_idx),
                        is_focused: Some(id) == focused_ws_id,
                        active_window_id: ws.active_window().map(|win| win.id().get()),
                    }
                })
                .collect();

            events.push(Event::WorkspacesChanged { workspaces });
        }

        for event in events {
            state.apply(event.clone());
            server.send_event(event);
        }
    }

    fn ipc_refresh_windows(&mut self) {
        let Some(server) = &self.niri.ipc_server else {
            return;
        };

        let _span = tracy_client::span!("State::ipc_refresh_windows");

        let mut state = server.event_stream_state.borrow_mut();
        let state = &mut state.windows;

        let mut events = Vec::new();
        let layout = &self.niri.layout;

        let mut batch_change_layouts: Vec<(u64, WindowLayout)> = Vec::new();

        // Check for window changes.
        let mut seen = HashSet::new();
        let mut focused_id = None;
        layout.with_windows(|mapped, _, ws_id, window_layout| {
            let id = mapped.id().get();
            seen.insert(id);

            if mapped.is_focused() {
                focused_id = Some(id);
            }

            let Some(ipc_win) = state.windows.get(&id) else {
                let window = make_ipc_window(mapped, ws_id, window_layout);
                events.push(Event::WindowOpenedOrChanged { window });
                return;
            };

            let workspace_id = ws_id.map(|id| id.get());
            let mut changed =
                ipc_win.workspace_id != workspace_id || ipc_win.is_floating != mapped.is_floating();

            changed |= with_toplevel_role(mapped.toplevel(), |role| {
                ipc_win.title != role.title || ipc_win.app_id != role.app_id
            });

            if changed {
                let window = make_ipc_window(mapped, ws_id, window_layout);
                events.push(Event::WindowOpenedOrChanged { window });
                return;
            }

            if ipc_win.layout != window_layout {
                batch_change_layouts.push((id, window_layout));
            }

            if mapped.is_focused() && !ipc_win.is_focused {
                events.push(Event::WindowFocusChanged { id: Some(id) });
            }

            let focus_timestamp = mapped.get_focus_timestamp().map(Timestamp::from);
            if focus_timestamp != ipc_win.focus_timestamp {
                events.push(Event::WindowFocusTimestampChanged {
                    id,
                    focus_timestamp,
                });
            }

            let urgent = mapped.is_urgent();
            if urgent != ipc_win.is_urgent {
                events.push(Event::WindowUrgencyChanged { id, urgent })
            }
        });

        // It might make sense to push layout changes after closed windows (since windows about to
        // be closed will occupy the same column/tile positions as the window that moved into this
        // vacated space), but also we are already pushing some layout changes in
        // WindowOpenedOrChanged above, meaning that the receiving end has to handle this case
        // anyway.
        if !batch_change_layouts.is_empty() {
            events.push(Event::WindowLayoutsChanged {
                changes: batch_change_layouts,
            });
        }

        // Check for closed windows.
        let mut ipc_focused_id = None;
        for (id, ipc_win) in &state.windows {
            if !seen.contains(id) {
                events.push(Event::WindowClosed { id: *id });
            }

            if ipc_win.is_focused {
                ipc_focused_id = Some(id);
            }
        }

        // Extra check for focus becoming None, since the checks above only work for focus becoming
        // a different window.
        if focused_id.is_none() && ipc_focused_id.is_some() {
            events.push(Event::WindowFocusChanged { id: None });
        }

        for event in events {
            state.apply(event.clone());
            server.send_event(event);
        }
    }

    pub fn ipc_refresh_overview(&mut self) {
        let Some(server) = &self.niri.ipc_server else {
            return;
        };

        let mut state = server.event_stream_state.borrow_mut();
        let state = &mut state.overview;
        let is_open = self.niri.layout.is_overview_open();

        if state.is_open == is_open {
            return;
        }

        let event = Event::OverviewOpenedOrClosed { is_open };
        state.apply(event.clone());
        server.send_event(event);
    }

    pub fn ipc_refresh_casts(&mut self) {
        let Some(server) = &self.niri.ipc_server else {
            return;
        };

        let _span = tracy_client::span!("State::ipc_refresh_casts");

        let mut state = server.event_stream_state.borrow_mut();
        let state = &mut state.casts;

        let mut events = Vec::new();
        let mut seen = HashSet::new();

        // Check PipeWire screencasts.
        #[cfg(feature = "xdp-gnome-screencast")]
        {
            // Check pending dynamic casts.
            for pending in &self.niri.casting.pending_dynamic_casts {
                let stream_id = pending.stream_id.get();
                seen.insert(stream_id);

                // Pending dynamic casts don't change any properties, so we only need to check if
                // it's missing from the state.
                if !state.casts.contains_key(&stream_id) {
                    let cast = niri_ipc::Cast {
                        session_id: pending.session_id.get(),
                        stream_id,
                        kind: niri_ipc::CastKind::PipeWire,
                        target: niri_ipc::CastTarget::Nothing {},
                        is_dynamic_target: true,
                        is_active: false,
                        pid: None,
                        pw_node_id: None,
                    };
                    events.push(Event::CastStartedOrChanged { cast });
                }
            }

            // Check active casts.
            for cast in &self.niri.casting.casts {
                let stream_id = cast.stream_id.get();
                seen.insert(stream_id);

                let pw_node_id = cast.node_id();
                if state.casts.get(&stream_id).is_none_or(|existing| {
                    // Only these properties can change.
                    existing.is_active != cast.is_active()
                        || !cast.target.matches(&existing.target)
                        || existing.pw_node_id != pw_node_id
                }) {
                    let cast = niri_ipc::Cast {
                        session_id: cast.session_id.get(),
                        stream_id,
                        kind: niri_ipc::CastKind::PipeWire,
                        target: cast.target.make_ipc(),
                        is_dynamic_target: cast.dynamic_target,
                        is_active: cast.is_active(),
                        pid: None,
                        pw_node_id,
                    };
                    events.push(Event::CastStartedOrChanged { cast });
                }
            }
        }

        // Check screencopy casts.
        //
        // First, clear expired casts. Ideally we'd have a deadline timer, but our 1 second frame
        // callback timer calls refresh regularly, so that's fine as is.
        self.niri.screencopy_state.clear_expired_casts();

        for queue in self.niri.screencopy_state.queues() {
            if let Some(cast_info) = queue.cast() {
                let stream_id = cast_info.stream_id.get();
                seen.insert(stream_id);

                if state.casts.get(&stream_id).is_none_or(|existing| {
                    // Only this property can change.
                    match &existing.target {
                        niri_ipc::CastTarget::Output { name } => *name != cast_info.output_name,
                        _ => true,
                    }
                }) {
                    let cast = niri_ipc::Cast {
                        session_id: cast_info.session_id.get(),
                        stream_id,
                        kind: niri_ipc::CastKind::WlrScreencopy,
                        target: niri_ipc::CastTarget::Output {
                            name: cast_info.output_name.clone(),
                        },
                        is_dynamic_target: false,
                        is_active: true,
                        pid: queue.credentials().map(|creds| creds.pid),
                        pw_node_id: None,
                    };
                    events.push(Event::CastStartedOrChanged { cast });
                }
            }
        }

        // Check ext-image-copy-capture casts.
        //
        // Unlike wlr-screencopy, a session is an explicit protocol object, so
        // the cast lifetime is tied to it. Dead sessions are already dropped by
        // refresh_image_copy_capture(), which runs before this.
        let output_sessions = self
            .niri
            .image_copy_sessions
            .iter()
            .map(|s| (s.session_id, s.stream_id, s.session.source(), s.credentials));
        let cursor_sessions = self
            .niri
            .image_copy_cursor_sessions
            .iter()
            .map(|s| (s.session_id, s.stream_id, s.session.source(), s.credentials));
        for (session_id, stream_id, source, credentials) in output_sessions.chain(cursor_sessions) {
            let Some(output) = image_copy_capture::source_output(&source) else {
                continue;
            };

            let stream_id = stream_id.get();
            seen.insert(stream_id);

            if !state.casts.contains_key(&stream_id) {
                let cast = niri_ipc::Cast {
                    session_id: session_id.get(),
                    stream_id,
                    kind: niri_ipc::CastKind::ExtImageCopyCapture,
                    target: niri_ipc::CastTarget::Output {
                        name: output.name(),
                    },
                    is_dynamic_target: false,
                    is_active: true,
                    pid: credentials.map(|creds| creds.pid),
                    pw_node_id: None,
                };
                events.push(Event::CastStartedOrChanged { cast });
            }
        }

        // Check for stopped casts.
        for stream_id in state.casts.keys() {
            if !seen.contains(stream_id) {
                events.push(Event::CastStopped {
                    stream_id: *stream_id,
                });
            }
        }

        for event in events {
            state.apply(event.clone());
            server.send_event(event);
        }
    }

    pub fn ipc_config_loaded(&mut self, failed: bool) {
        let Some(server) = &self.niri.ipc_server else {
            return;
        };
        let mut state = server.event_stream_state.borrow_mut();

        let event = Event::ConfigLoaded { failed };
        state.apply(event.clone());
        server.send_event(event);
    }

    pub fn ipc_screenshot_taken(&mut self, path: Option<String>) {
        let Some(server) = &self.niri.ipc_server else {
            return;
        };
        let mut state = server.event_stream_state.borrow_mut();

        let event = Event::ScreenshotCaptured { path };
        state.apply(event.clone());
        server.send_event(event);
    }

    /// Announce that a touchscreen gesture has started.
    ///
    /// Purely informational: this never fires the gesture's bind. It is
    /// only ever called for a bind configured with `emit-progress true`.
    pub fn ipc_touch_gesture_begin(
        &mut self,
        trigger: String,
        finger_count: u8,
        displacement: f64,
    ) {
        self.ipc_touch_event(
            Event::TouchGestureBegin {
                trigger,
                finger_count,
                displacement,
            },
            true,
        );
    }

    /// Report live progress of a touchscreen gesture.
    pub fn ipc_touch_gesture_update(
        &mut self,
        trigger: String,
        progress_delta: f64,
        progress: f64,
        velocity: f64,
        displacement: f64,
    ) {
        self.ipc_touch_event(
            Event::TouchGestureUpdate {
                trigger,
                progress_delta,
                progress,
                velocity,
                displacement,
            },
            true,
        );
    }

    /// Report that a touchscreen gesture has finished.
    pub fn ipc_touch_gesture_end(
        &mut self,
        trigger: String,
        progress: f64,
        committed: bool,
        displacement: f64,
    ) {
        self.ipc_touch_event(
            Event::TouchGestureEnd {
                trigger,
                progress,
                committed,
                displacement,
            },
            true,
        );
    }

    /// The single place a touch event is published.
    ///
    /// Every one of the six touch [`Event`] variants goes through here, so the
    /// event-stream clients and the [`Request::TouchTrace`] ring buffer can
    /// never see different events: there is exactly one publish point, and it
    /// writes both.
    ///
    /// `broadcast` gates only the live event stream, never the ring buffer.
    /// The ring is always populated, so a client can recover evidence from a
    /// session that was never started with `NIRI_IPC_TOUCH_TRACE=1` — which is
    /// the only case worth recovering evidence for, since you cannot know in
    /// advance which session will misbehave. The stream is the part that can
    /// flood a real subscriber with one line per finger-down, so it stays
    /// opt-in.
    ///
    /// None of these events is state that a newly connected client needs
    /// replayed, so `EventStreamState::apply` passes them straight through; it
    /// is still called so that a future state part gets a chance to see them.
    fn ipc_touch_event(&mut self, event: Event, broadcast: bool) {
        let Some(server) = &self.niri.ipc_server else {
            return;
        };

        // Cheap: one clone into a pre-reserved slot, no allocation and no
        // lock (the ring is main-loop-only, like the rest of the server state).
        // The ring is written whether or not the stream is gated; that is what
        // keeps `Request::TouchTrace` useful without `NIRI_IPC_TOUCH_TRACE=1`.
        let Some(event) =
            record_touch_event(&mut server.touch_trace.borrow_mut(), event, broadcast)
        else {
            return;
        };

        let mut state = server.event_stream_state.borrow_mut();
        state.apply(event.clone());
        server.send_event(event);
    }

    /// Publish one touch-ownership decision, if the trace is enabled.
    #[expect(
        clippy::too_many_arguments,
        reason = "one field per documented event field"
    )]
    pub fn ipc_touch_arbitration_decision(
        &mut self,
        slot: u32,
        ownership: TouchOwnership,
        rule: String,
        finger_count: u32,
        arming_eligible: u32,
        open_client_touches: u32,
        sequence_compositor_owned: bool,
        is_pen: bool,
        pos_x: f64,
        pos_y: f64,
    ) {
        self.ipc_touch_event(
            Event::TouchArbitrationDecision {
                slot,
                ownership,
                rule,
                finger_count,
                arming_eligible,
                open_client_touches,
                sequence_compositor_owned,
                is_pen,
                pos_x,
                pos_y,
            },
            self.touch_trace_enabled(),
        );
    }

    /// Publish the retirement of one contact from the touch ledger.
    pub fn ipc_touch_slot_retired(&mut self, slot: u32, was_tracked: bool) {
        self.ipc_touch_event(
            Event::TouchSlotRetired { slot, was_tracked },
            self.touch_trace_enabled(),
        );
    }

    /// Publish a wholesale clear of the touch ledger.
    pub fn ipc_touch_sequence_reset(&mut self, open_client_touches: u32, live_contacts: u32) {
        self.ipc_touch_event(
            Event::TouchSequenceReset {
                open_client_touches,
                live_contacts,
            },
            self.touch_trace_enabled(),
        );
    }

    /// Whether the compositor was started with the touch trace enabled.
    fn touch_trace_enabled(&self) -> bool {
        self.niri
            .ipc_server
            .as_ref()
            .is_some_and(|server| server.trace_touch_arbitration)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(slot: u32) -> Event {
        Event::TouchArbitrationDecision {
            slot,
            ownership: TouchOwnership::Compositor,
            rule: "8_arming".to_string(),
            finger_count: 3,
            arming_eligible: 3,
            open_client_touches: 0,
            sequence_compositor_owned: false,
            is_pen: false,
            pos_x: 100.0 + slot as f64,
            pos_y: 200.0,
        }
    }

    fn slot_of(entry: &TouchTraceEntry) -> u32 {
        match entry.event {
            Event::TouchArbitrationDecision { slot, .. } => slot,
            _ => panic!("not an arbitration decision"),
        }
    }

    #[test]
    fn touch_trace_ring_is_empty_at_start() {
        let ring = TouchTraceRing::new();
        let trace = ring.since(0);
        assert!(trace.entries.is_empty());
        assert_eq!(trace.next_sequence, 1);
    }

    #[test]
    fn touch_trace_ring_sequences_are_monotonic_from_one() {
        let mut ring = TouchTraceRing::new();
        for slot in 0..5 {
            ring.push(event(slot));
        }

        let trace = ring.since(0);
        assert_eq!(
            trace.entries.iter().map(|e| e.sequence).collect::<Vec<_>>(),
            vec![1, 2, 3, 4, 5]
        );
        assert_eq!(trace.next_sequence, 6);
    }

    #[test]
    fn touch_trace_ring_since_filters_strictly_greater() {
        let mut ring = TouchTraceRing::new();
        for slot in 0..5 {
            ring.push(event(slot));
        }

        // `since` is exclusive: an entry with exactly this sequence was already seen.
        assert!(ring.since(5).entries.is_empty());

        let trace = ring.since(3);
        assert_eq!(
            trace.entries.iter().map(|e| e.sequence).collect::<Vec<_>>(),
            vec![4, 5]
        );
        // next_sequence is the overall high-water mark, not derived from `since`.
        assert_eq!(trace.next_sequence, 6);
    }

    #[test]
    fn touch_trace_ring_bounds_at_capacity_and_drops_oldest() {
        let mut ring = TouchTraceRing::new();
        let overflow = 10;
        let total = TOUCH_TRACE_CAPACITY + overflow;
        for slot in 0..total {
            ring.push(event(slot as u32));
        }

        let trace = ring.since(0);
        assert_eq!(trace.entries.len(), TOUCH_TRACE_CAPACITY);
        assert_eq!(trace.next_sequence, total as u64 + 1);

        // The oldest `overflow` entries are the ones dropped: what is left is
        // exactly the tail of the stream, oldest first.
        assert_eq!(
            trace.entries.iter().map(|e| e.sequence).collect::<Vec<_>>(),
            (overflow as u64 + 1..=total as u64).collect::<Vec<_>>()
        );
        assert_eq!(
            trace.entries.iter().map(slot_of).collect::<Vec<_>>(),
            (overflow as u32..total as u32).collect::<Vec<_>>()
        );

        // A `since` older than the buffer still returns the whole buffer, never
        // more than capacity, and never panics.
        assert_eq!(ring.since(0).entries.len(), TOUCH_TRACE_CAPACITY);
        assert_eq!(
            ring.since(u64::MAX).entries.len(),
            0,
            "a client ahead of the compositor gets nothing back"
        );
    }

    /// The regression that matters: the ring must be populated even with
    /// `NIRI_IPC_TOUCH_TRACE` unset.
    ///
    /// A cargo test run has no env var set, so this is exactly the ordinary
    /// session that the ring exists for. If `broadcast` were allowed to skip
    /// the ring, `Request::TouchTrace` would return `[]` in every session that
    /// did not opt in — inert by default, and useless for the only case worth
    /// debugging (the faulting session you did not predict).
    #[test]
    fn touch_trace_ring_is_populated_with_the_env_var_unset() {
        assert!(
            env::var_os("NIRI_IPC_TOUCH_TRACE").is_none(),
            "this test asserts the untraced path; do not run it with the env var set"
        );

        let mut ring = TouchTraceRing::new();

        // broadcast = false is exactly what `touch_trace_enabled()` returns
        // when the env var is unset.
        let streamed = record_touch_event(&mut ring, event(1), false);
        assert!(
            streamed.is_none(),
            "with the trace off the event must not reach the live stream"
        );

        // ...but it is still in the ring.
        let trace = ring.since(0);
        assert_eq!(
            trace.entries.len(),
            1,
            "the ring must record even with the stream gated off"
        );
        assert_eq!(slot_of(&trace.entries[0]), 1);
        assert_eq!(trace.next_sequence, 2);
    }

    /// The other half of the split: with the trace on, the stream still gets
    /// the event. The stream gate must not have been broken in fixing the ring.
    #[test]
    fn touch_trace_ring_still_broadcasts_when_the_env_var_is_set() {
        let mut ring = TouchTraceRing::new();

        let streamed = record_touch_event(&mut ring, event(2), true);
        assert!(
            streamed.is_some(),
            "with the trace on the event must still reach the live stream"
        );
        assert_eq!(ring.since(0).entries.len(), 1);
    }

    /// Ring and stream see the same events, because both are written from the
    /// one publish function — there is no path that reaches one and not the
    /// other, whichever way the gate is set.
    #[test]
    fn ring_and_stream_never_disagree() {
        for broadcast in [false, true] {
            let mut ring = TouchTraceRing::new();
            let mut streamed = Vec::new();

            for slot in 0..4 {
                if let Some(event) = record_touch_event(&mut ring, event(slot), broadcast) {
                    streamed.push(event);
                }
            }

            let entries = ring.since(0).entries;
            assert_eq!(entries.len(), 4, "ring saw a different set");
            assert_eq!(
                streamed.len(),
                if broadcast { 4 } else { 0 },
                "the stream saw the wrong number of events for broadcast={broadcast}"
            );
            for (entry, event) in entries.iter().map(|e| &e.event).zip(&streamed) {
                assert_eq!(
                    serde_json::to_string(entry).unwrap(),
                    serde_json::to_string(event).unwrap(),
                    "ring and stream disagreed on the event contents"
                );
            }
        }
    }
}

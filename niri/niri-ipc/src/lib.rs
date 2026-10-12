//! Types for communicating with niri via IPC.
//!
//! After connecting to the niri socket, you can send [`Request`]s. Niri will process them one by
//! one, in order, and to each request it will respond with a single [`Reply`], which is a `Result`
//! wrapping a [`Response`].
//!
//! If you send a [`Request::EventStream`], niri will *stop* reading subsequent [`Request`]s, and
//! will start continuously writing compositor [`Event`]s to the socket. If you'd like to read an
//! event stream and write more requests at the same time, you need to use two IPC sockets.
//!
//! <div class="warning">
//!
//! Requests are *always* processed separately. Time passes between requests, even when sending
//! multiple requests to the socket at once. For example, sending [`Request::Workspaces`] and
//! [`Request::Windows`] together may not return consistent results (e.g. a window may open on a
//! new workspace in-between the two responses). This goes for actions too: sending
//! [`Action::FocusWindow`] and <code>[Action::CloseWindow] { id: None }</code> together may close
//! the wrong window because a different window got focused in-between these requests.
//!
//! </div>
//!
//! You can use the [`socket::Socket`] helper if you're fine with blocking communication. However,
//! it is a fairly simple helper, so if you need async, or if you're using a different language,
//! you are encouraged to communicate with the socket manually.
//!
//! 1. Read the socket filesystem path from [`socket::SOCKET_PATH_ENV`] (`$NIRI_SOCKET`).
//! 2. Connect to the socket and write a JSON-formatted [`Request`] on a single line. You can follow
//!    up with a line break and a flush, or just flush and shutdown the write end of the socket.
//! 3. Niri will respond with a single line JSON-formatted [`Reply`].
//! 4. You can keep writing [`Request`]s, each on a single line, and read [`Reply`]s, also each on a
//!    separate line.
//! 5. After you request an event stream, niri will keep responding with JSON-formatted [`Event`]s,
//!    on a single line each.
//!
//! ## Backwards compatibility
//!
//! This crate follows the niri version. It is **not** API-stable in terms of the Rust semver. In
//! particular, expect new struct fields and enum variants to be added in patch version bumps.
//!
//! Use an exact version requirement to avoid breaking changes:
//!
//! ```toml
//! [dependencies]
//! niri-ipc = "=26.4.0"
//! ```
//!
//! ## Features
//!
//! This crate defines the following features:
//! - `json-schema`: derives the [schemars](https://lib.rs/crates/schemars) `JsonSchema` trait for
//!   the types.
//! - `clap`: derives the clap CLI parsing traits for some types. Used internally by niri itself.
#![warn(missing_docs)]

use std::collections::HashMap;
use std::str::FromStr;
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub mod socket;
pub mod state;

/// Request from client to niri.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub enum Request {
    /// Request the version string for the running niri instance.
    Version,
    /// Request information about connected outputs.
    Outputs,
    /// Request information about workspaces.
    Workspaces,
    /// Request information about open windows.
    Windows,
    /// Request information about layer-shell surfaces.
    Layers,
    /// Request information about the configured keyboard layouts.
    KeyboardLayouts,
    /// Request information about the focused output.
    FocusedOutput,
    /// Request information about the focused window.
    FocusedWindow,
    /// Request picking a window and get its information.
    PickWindow,
    /// Request picking a color from the screen.
    PickColor,
    /// Perform an action.
    Action(Action),
    /// Change output configuration temporarily.
    ///
    /// The configuration is changed temporarily and not saved into the config file. If the output
    /// configuration subsequently changes in the config file, these temporary changes will be
    /// forgotten.
    Output {
        /// Output name.
        output: String,
        /// Configuration to apply.
        action: OutputAction,
    },
    /// Start continuously receiving events from the compositor.
    ///
    /// The compositor should reply with `Reply::Ok(Response::Handled)`, then continuously send
    /// [`Event`]s, one per line.
    ///
    /// The event stream will always give you the full current state up-front. For example, the
    /// first workspace-related event you will receive will be [`Event::WorkspacesChanged`]
    /// containing the full current workspaces state. You *do not* need to separately send
    /// [`Request::Workspaces`] when using the event stream.
    ///
    /// Where reasonable, event stream state updates are atomic, though this is not always the
    /// case. For example, a window may end up with a workspace id for a workspace that had already
    /// been removed. This can happen if the corresponding [`Event::WorkspacesChanged`] arrives
    /// before the corresponding [`Event::WindowOpenedOrChanged`].
    EventStream,
    /// Respond with an error (for testing error handling).
    ReturnError,
    /// Request information about the overview.
    OverviewState,
    /// Request information about screencasts.
    Casts,
    /// Read back buffered touch-trace events.
    ///
    /// The touch events on the event stream are push-only: a client that
    /// connects after a gesture has already misbehaved has missed them. The
    /// compositor keeps a bounded ring buffer of the most recent touch events
    /// and this request reads it back, so a client can poll incrementally.
    /// The ring is always populated — it does not require
    /// `NIRI_IPC_TOUCH_TRACE=1` or any per-bind `emit-progress` flag. Those
    /// gate the live event stream only. So this works in an ordinary session,
    /// which is what makes it useful: you cannot know in advance which session
    /// will misbehave.
    ///
    /// `since` is the highest sequence number the client has already seen
    /// (0 for a client that has seen nothing). Only entries with a strictly
    /// greater sequence number are returned, oldest first.
    ///
    /// This is deliberately a short recent history, not a replay log: entries
    /// are dropped once the buffer is full.
    TouchTrace {
        /// Highest sequence number the client has already seen.
        since: u64,
    },
}

/// Reply from niri to client.
///
/// Every request gets one reply.
///
/// * If an error had occurred, it will be an `Reply::Err`.
/// * If the request does not need any particular response, it will be
///   `Reply::Ok(Response::Handled)`. Kind of like an `Ok(())`.
/// * Otherwise, it will be `Reply::Ok(response)` with one of the other [`Response`] variants.
pub type Reply = Result<Response, String>;

/// Successful response from niri to client.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub enum Response {
    /// A request that does not need a response was handled successfully.
    Handled,
    /// The version string for the running niri instance.
    Version(String),
    /// Information about connected outputs.
    ///
    /// Map from output name to output info.
    Outputs(HashMap<String, Output>),
    /// Information about workspaces.
    Workspaces(Vec<Workspace>),
    /// Information about open windows.
    Windows(Vec<Window>),
    /// Information about layer-shell surfaces.
    Layers(Vec<LayerSurface>),
    /// Information about the keyboard layout.
    KeyboardLayouts(KeyboardLayouts),
    /// Information about the focused output.
    FocusedOutput(Option<Output>),
    /// Information about the focused window.
    FocusedWindow(Option<Window>),
    /// Information about the picked window.
    PickedWindow(Option<Window>),
    /// Information about the picked color.
    PickedColor(Option<PickedColor>),
    /// Output configuration change result.
    OutputConfigChanged(OutputConfigChanged),
    /// Information about the overview.
    OverviewState(Overview),
    /// Information about screencasts.
    Casts(Vec<Cast>),
    /// A read-back of buffered touch-trace events.
    ///
    /// See [`Request::TouchTrace`].
    TouchTrace(TouchTrace),
}

/// Overview information.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct Overview {
    /// Whether the overview is currently open.
    pub is_open: bool,
}

/// Color picked from the screen.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct PickedColor {
    /// Color values as red, green, blue, each ranging from 0.0 to 1.0.
    pub rgb: [f64; 3],
}

/// Actions that niri can perform.
// Variants in this enum should match the spelling of the ones in niri-config. Most, but not all,
// variants from niri-config should be present here.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "clap", derive(clap::Parser))]
#[cfg_attr(feature = "clap", command(subcommand_value_name = "ACTION"))]
#[cfg_attr(feature = "clap", command(subcommand_help_heading = "Actions"))]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub enum Action {
    /// Exit niri.
    Quit {
        /// Skip the "Press Enter to confirm" prompt.
        #[cfg_attr(feature = "clap", arg(short, long))]
        skip_confirmation: bool,
    },
    /// Power off all monitors via DPMS.
    PowerOffMonitors {},
    /// Power on all monitors via DPMS.
    PowerOnMonitors {},
    /// Spawn a command.
    Spawn {
        /// Command to spawn.
        #[cfg_attr(feature = "clap", arg(last = true, required = true))]
        command: Vec<String>,
    },
    /// Spawn a command through the shell.
    SpawnSh {
        /// Command to run.
        #[cfg_attr(feature = "clap", arg(last = true, required = true))]
        command: String,
    },
    /// Do a screen transition.
    DoScreenTransition {
        /// Delay in milliseconds for the screen to freeze before starting the transition.
        #[cfg_attr(feature = "clap", arg(short, long))]
        delay_ms: Option<u16>,
    },
    /// Open the screenshot UI.
    Screenshot {
        ///  Whether to show the mouse pointer by default in the screenshot UI.
        #[cfg_attr(feature = "clap", arg(short = 'p', long, action = clap::ArgAction::Set, default_value_t = true))]
        show_pointer: bool,

        /// Path to save the screenshot to.
        ///
        /// The path must be absolute, otherwise an error is returned.
        ///
        /// If `None`, the screenshot is saved according to the `screenshot-path` config setting.
        #[cfg_attr(feature = "clap", arg(long, action = clap::ArgAction::Set))]
        path: Option<String>,
    },
    /// Screenshot the focused screen.
    ScreenshotScreen {
        /// Write the screenshot to disk in addition to putting it in your clipboard.
        ///
        /// The screenshot is saved according to the `screenshot-path` config setting.
        #[cfg_attr(feature = "clap", arg(short = 'd', long, action = clap::ArgAction::Set, default_value_t = true))]
        write_to_disk: bool,

        /// Whether to include the mouse pointer in the screenshot.
        #[cfg_attr(feature = "clap", arg(short = 'p', long, action = clap::ArgAction::Set, default_value_t = true))]
        show_pointer: bool,

        /// Path to save the screenshot to.
        ///
        /// The path must be absolute, otherwise an error is returned.
        ///
        /// If `None`, the screenshot is saved according to the `screenshot-path` config setting.
        #[cfg_attr(feature = "clap", arg(long, action = clap::ArgAction::Set))]
        path: Option<String>,
    },
    /// Screenshot a window.
    #[cfg_attr(feature = "clap", clap(about = "Screenshot the focused window"))]
    ScreenshotWindow {
        /// Id of the window to screenshot.
        ///
        /// If `None`, uses the focused window.
        #[cfg_attr(feature = "clap", arg(long))]
        id: Option<u64>,
        /// Write the screenshot to disk in addition to putting it in your clipboard.
        ///
        /// The screenshot is saved according to the `screenshot-path` config setting.
        #[cfg_attr(feature = "clap", arg(short = 'd', long, action = clap::ArgAction::Set, default_value_t = true))]
        write_to_disk: bool,

        /// Whether to include the mouse pointer in the screenshot.
        ///
        /// The pointer will be included only if the window is currently receiving pointer input
        /// (usually this means the pointer is on top of the window).
        #[cfg_attr(feature = "clap", arg(short = 'p', long, action = clap::ArgAction::Set, default_value_t = false))]
        show_pointer: bool,

        /// Path to save the screenshot to.
        ///
        /// The path must be absolute, otherwise an error is returned.
        ///
        /// If `None`, the screenshot is saved according to the `screenshot-path` config setting.
        #[cfg_attr(feature = "clap", arg(long, action = clap::ArgAction::Set))]
        path: Option<String>,
    },
    /// Enable or disable the keyboard shortcuts inhibitor (if any) for the focused surface.
    ToggleKeyboardShortcutsInhibit {},
    /// Close a window.
    #[cfg_attr(feature = "clap", clap(about = "Close the focused window"))]
    CloseWindow {
        /// Id of the window to close.
        ///
        /// If `None`, uses the focused window.
        #[cfg_attr(feature = "clap", arg(long))]
        id: Option<u64>,
    },
    /// Toggle fullscreen on a window.
    #[cfg_attr(
        feature = "clap",
        clap(about = "Toggle fullscreen on the focused window")
    )]
    FullscreenWindow {
        /// Id of the window to toggle fullscreen of.
        ///
        /// If `None`, uses the focused window.
        #[cfg_attr(feature = "clap", arg(long))]
        id: Option<u64>,
    },
    /// Toggle windowed (fake) fullscreen on a window.
    #[cfg_attr(
        feature = "clap",
        clap(about = "Toggle windowed (fake) fullscreen on the focused window")
    )]
    ToggleWindowedFullscreen {
        /// Id of the window to toggle windowed fullscreen of.
        ///
        /// If `None`, uses the focused window.
        #[cfg_attr(feature = "clap", arg(long))]
        id: Option<u64>,
    },
    /// Focus a window by id.
    FocusWindow {
        /// Id of the window to focus.
        #[cfg_attr(feature = "clap", arg(long))]
        id: u64,
    },
    /// Focus a window in the focused column by index.
    FocusWindowInColumn {
        /// Index of the window in the column.
        ///
        /// The index starts from 1 for the topmost window.
        #[cfg_attr(feature = "clap", arg())]
        index: u8,
    },
    /// Focus the previously focused window.
    FocusWindowPrevious {},
    /// Focus the column to the left.
    FocusColumnLeft {},
    /// Focus the column to the right.
    FocusColumnRight {},
    /// Focus the first column.
    FocusColumnFirst {},
    /// Focus the last column.
    FocusColumnLast {},
    /// Focus the next column to the right, looping if at end.
    FocusColumnRightOrFirst {},
    /// Focus the next column to the left, looping if at start.
    FocusColumnLeftOrLast {},
    /// Focus a column by index.
    FocusColumn {
        /// Index of the column to focus.
        ///
        /// The index starts from 1 for the first column.
        #[cfg_attr(feature = "clap", arg())]
        index: usize,
    },
    /// Focus the window or the monitor above.
    FocusWindowOrMonitorUp {},
    /// Focus the window or the monitor below.
    FocusWindowOrMonitorDown {},
    /// Focus the column or the monitor to the left.
    FocusColumnOrMonitorLeft {},
    /// Focus the column or the monitor to the right.
    FocusColumnOrMonitorRight {},
    /// Focus the window below.
    FocusWindowDown {},
    /// Focus the window above.
    FocusWindowUp {},
    /// Focus the window below or the column to the left.
    FocusWindowDownOrColumnLeft {},
    /// Focus the window below or the column to the right.
    FocusWindowDownOrColumnRight {},
    /// Focus the window above or the column to the left.
    FocusWindowUpOrColumnLeft {},
    /// Focus the window above or the column to the right.
    FocusWindowUpOrColumnRight {},
    /// Focus the window or the workspace below.
    FocusWindowOrWorkspaceDown {},
    /// Focus the window or the workspace above.
    FocusWindowOrWorkspaceUp {},
    /// Focus the topmost window.
    FocusWindowTop {},
    /// Focus the bottommost window.
    FocusWindowBottom {},
    /// Focus the window below or the topmost window.
    FocusWindowDownOrTop {},
    /// Focus the window above or the bottommost window.
    FocusWindowUpOrBottom {},
    /// Move the focused column to the left.
    MoveColumnLeft {},
    /// Move the focused column to the right.
    MoveColumnRight {},
    /// Move the focused column to the start of the workspace.
    MoveColumnToFirst {},
    /// Move the focused column to the end of the workspace.
    MoveColumnToLast {},
    /// Move the focused column to the left or to the monitor to the left.
    MoveColumnLeftOrToMonitorLeft {},
    /// Move the focused column to the right or to the monitor to the right.
    MoveColumnRightOrToMonitorRight {},
    /// Move the focused column to a specific index on its workspace.
    MoveColumnToIndex {
        /// New index for the column.
        ///
        /// The index starts from 1 for the first column.
        #[cfg_attr(feature = "clap", arg())]
        index: usize,
    },
    /// Move the focused window down in a column.
    MoveWindowDown {},
    /// Move the focused window up in a column.
    MoveWindowUp {},
    /// Move the focused window down in a column or to the workspace below.
    MoveWindowDownOrToWorkspaceDown {},
    /// Move the focused window up in a column or to the workspace above.
    MoveWindowUpOrToWorkspaceUp {},
    /// Consume or expel a window left.
    #[cfg_attr(
        feature = "clap",
        clap(about = "Consume or expel the focused window left")
    )]
    ConsumeOrExpelWindowLeft {
        /// Id of the window to consume or expel.
        ///
        /// If `None`, uses the focused window.
        #[cfg_attr(feature = "clap", arg(long))]
        id: Option<u64>,
    },
    /// Consume or expel a window right.
    #[cfg_attr(
        feature = "clap",
        clap(about = "Consume or expel the focused window right")
    )]
    ConsumeOrExpelWindowRight {
        /// Id of the window to consume or expel.
        ///
        /// If `None`, uses the focused window.
        #[cfg_attr(feature = "clap", arg(long))]
        id: Option<u64>,
    },
    /// Consume the window to the right into the focused column.
    ConsumeWindowIntoColumn {},
    /// Expel the bottom window from the focused column.
    ExpelWindowFromColumn {},
    /// Swap focused window with one to the right.
    SwapWindowRight {},
    /// Swap focused window with one to the left.
    SwapWindowLeft {},
    /// Toggle the focused column between normal and tabbed display.
    ToggleColumnTabbedDisplay {},
    /// Set the display mode of the focused column.
    SetColumnDisplay {
        /// Display mode to set.
        #[cfg_attr(feature = "clap", arg())]
        display: ColumnDisplay,
    },
    /// Center the focused column on the screen.
    CenterColumn {},
    /// Center a window on the screen.
    #[cfg_attr(
        feature = "clap",
        clap(about = "Center the focused window on the screen")
    )]
    CenterWindow {
        /// Id of the window to center.
        ///
        /// If `None`, uses the focused window.
        #[cfg_attr(feature = "clap", arg(long))]
        id: Option<u64>,
    },
    /// Center all fully visible columns on the screen.
    CenterVisibleColumns {},
    /// Focus the workspace below.
    FocusWorkspaceDown {},
    /// Focus the workspace above.
    FocusWorkspaceUp {},
    /// Focus a workspace by reference (index or name).
    FocusWorkspace {
        /// Reference (index or name) of the workspace to focus.
        #[cfg_attr(feature = "clap", arg())]
        reference: WorkspaceReferenceArg,
    },
    /// Focus the previous workspace.
    FocusWorkspacePrevious {},
    /// Move the focused window to the workspace below.
    MoveWindowToWorkspaceDown {
        /// Whether the focus should follow the target workspace.
        ///
        /// If `true` (the default), the focus will follow the window to the new workspace. If
        /// `false`, the focus will remain on the original workspace.
        #[cfg_attr(feature = "clap", arg(long, action = clap::ArgAction::Set, default_value_t = true))]
        focus: bool,
    },
    /// Move the focused window to the workspace above.
    MoveWindowToWorkspaceUp {
        /// Whether the focus should follow the target workspace.
        ///
        /// If `true` (the default), the focus will follow the window to the new workspace. If
        /// `false`, the focus will remain on the original workspace.
        #[cfg_attr(feature = "clap", arg(long, action = clap::ArgAction::Set, default_value_t = true))]
        focus: bool,
    },
    /// Move a window to a workspace.
    #[cfg_attr(
        feature = "clap",
        clap(about = "Move the focused window to a workspace by reference (index or name)")
    )]
    MoveWindowToWorkspace {
        /// Id of the window to move.
        ///
        /// If `None`, uses the focused window.
        #[cfg_attr(feature = "clap", arg(long))]
        window_id: Option<u64>,

        /// Reference (index or name) of the workspace to move the window to.
        #[cfg_attr(feature = "clap", arg())]
        reference: WorkspaceReferenceArg,

        /// Whether the focus should follow the moved window.
        ///
        /// If `true` (the default) and the window to move is focused, the focus will follow the
        /// window to the new workspace. If `false`, the focus will remain on the original
        /// workspace.
        #[cfg_attr(feature = "clap", arg(long, action = clap::ArgAction::Set, default_value_t = true))]
        focus: bool,
    },
    /// Move the focused column to the workspace below.
    MoveColumnToWorkspaceDown {
        /// Whether the focus should follow the target workspace.
        ///
        /// If `true` (the default), the focus will follow the column to the new workspace. If
        /// `false`, the focus will remain on the original workspace.
        #[cfg_attr(feature = "clap", arg(long, action = clap::ArgAction::Set, default_value_t = true))]
        focus: bool,
    },
    /// Move the focused column to the workspace above.
    MoveColumnToWorkspaceUp {
        /// Whether the focus should follow the target workspace.
        ///
        /// If `true` (the default), the focus will follow the column to the new workspace. If
        /// `false`, the focus will remain on the original workspace.
        #[cfg_attr(feature = "clap", arg(long, action = clap::ArgAction::Set, default_value_t = true))]
        focus: bool,
    },
    /// Move the focused column to a workspace by reference (index or name).
    MoveColumnToWorkspace {
        /// Reference (index or name) of the workspace to move the column to.
        #[cfg_attr(feature = "clap", arg())]
        reference: WorkspaceReferenceArg,

        /// Whether the focus should follow the target workspace.
        ///
        /// If `true` (the default), the focus will follow the column to the new workspace. If
        /// `false`, the focus will remain on the original workspace.
        #[cfg_attr(feature = "clap", arg(long, action = clap::ArgAction::Set, default_value_t = true))]
        focus: bool,
    },
    /// Move the focused workspace down.
    MoveWorkspaceDown {},
    /// Move the focused workspace up.
    MoveWorkspaceUp {},
    /// Move a workspace to a specific index on its monitor.
    #[cfg_attr(
        feature = "clap",
        clap(about = "Move the focused workspace to a specific index on its monitor")
    )]
    MoveWorkspaceToIndex {
        /// New index for the workspace.
        #[cfg_attr(feature = "clap", arg())]
        index: usize,

        /// Reference (index or name) of the workspace to move.
        ///
        /// If `None`, uses the focused workspace.
        #[cfg_attr(feature = "clap", arg(long))]
        reference: Option<WorkspaceReferenceArg>,
    },
    /// Set the name of a workspace.
    #[cfg_attr(
        feature = "clap",
        clap(about = "Set the name of the focused workspace")
    )]
    SetWorkspaceName {
        /// New name for the workspace.
        #[cfg_attr(feature = "clap", arg())]
        name: String,

        /// Reference (index or name) of the workspace to name.
        ///
        /// If `None`, uses the focused workspace.
        #[cfg_attr(feature = "clap", arg(long))]
        workspace: Option<WorkspaceReferenceArg>,
    },
    /// Unset the name of a workspace.
    #[cfg_attr(
        feature = "clap",
        clap(about = "Unset the name of the focused workspace")
    )]
    UnsetWorkspaceName {
        /// Reference (index or name) of the workspace to unname.
        ///
        /// If `None`, uses the focused workspace.
        #[cfg_attr(feature = "clap", arg())]
        reference: Option<WorkspaceReferenceArg>,
    },
    /// Focus the monitor to the left.
    FocusMonitorLeft {},
    /// Focus the monitor to the right.
    FocusMonitorRight {},
    /// Focus the monitor below.
    FocusMonitorDown {},
    /// Focus the monitor above.
    FocusMonitorUp {},
    /// Focus the previous monitor.
    FocusMonitorPrevious {},
    /// Focus the next monitor.
    FocusMonitorNext {},
    /// Focus a monitor by name.
    FocusMonitor {
        /// Name of the output to focus.
        #[cfg_attr(feature = "clap", arg())]
        output: String,
    },
    /// Move the focused window to the monitor to the left.
    MoveWindowToMonitorLeft {},
    /// Move the focused window to the monitor to the right.
    MoveWindowToMonitorRight {},
    /// Move the focused window to the monitor below.
    MoveWindowToMonitorDown {},
    /// Move the focused window to the monitor above.
    MoveWindowToMonitorUp {},
    /// Move the focused window to the previous monitor.
    MoveWindowToMonitorPrevious {},
    /// Move the focused window to the next monitor.
    MoveWindowToMonitorNext {},
    /// Move a window to a specific monitor.
    #[cfg_attr(
        feature = "clap",
        clap(about = "Move the focused window to a specific monitor")
    )]
    MoveWindowToMonitor {
        /// Id of the window to move.
        ///
        /// If `None`, uses the focused window.
        #[cfg_attr(feature = "clap", arg(long))]
        id: Option<u64>,

        /// The target output name.
        #[cfg_attr(feature = "clap", arg())]
        output: String,
    },
    /// Move the focused column to the monitor to the left.
    MoveColumnToMonitorLeft {},
    /// Move the focused column to the monitor to the right.
    MoveColumnToMonitorRight {},
    /// Move the focused column to the monitor below.
    MoveColumnToMonitorDown {},
    /// Move the focused column to the monitor above.
    MoveColumnToMonitorUp {},
    /// Move the focused column to the previous monitor.
    MoveColumnToMonitorPrevious {},
    /// Move the focused column to the next monitor.
    MoveColumnToMonitorNext {},
    /// Move the focused column to a specific monitor.
    MoveColumnToMonitor {
        /// The target output name.
        #[cfg_attr(feature = "clap", arg())]
        output: String,
    },
    /// Change the width of a window.
    #[cfg_attr(
        feature = "clap",
        clap(about = "Change the width of the focused window")
    )]
    SetWindowWidth {
        /// Id of the window whose width to set.
        ///
        /// If `None`, uses the focused window.
        #[cfg_attr(feature = "clap", arg(long))]
        id: Option<u64>,

        /// How to change the width.
        #[cfg_attr(feature = "clap", arg(allow_hyphen_values = true))]
        change: SizeChange,
    },
    /// Change the height of a window.
    #[cfg_attr(
        feature = "clap",
        clap(about = "Change the height of the focused window")
    )]
    SetWindowHeight {
        /// Id of the window whose height to set.
        ///
        /// If `None`, uses the focused window.
        #[cfg_attr(feature = "clap", arg(long))]
        id: Option<u64>,

        /// How to change the height.
        #[cfg_attr(feature = "clap", arg(allow_hyphen_values = true))]
        change: SizeChange,
    },
    /// Reset the height of a window back to automatic.
    #[cfg_attr(
        feature = "clap",
        clap(about = "Reset the height of the focused window back to automatic")
    )]
    ResetWindowHeight {
        /// Id of the window whose height to reset.
        ///
        /// If `None`, uses the focused window.
        #[cfg_attr(feature = "clap", arg(long))]
        id: Option<u64>,
    },
    /// Switch between preset column widths.
    SwitchPresetColumnWidth {},
    /// Switch between preset column widths backwards.
    SwitchPresetColumnWidthBack {},
    /// Switch between preset window widths.
    SwitchPresetWindowWidth {
        /// Id of the window whose width to switch.
        ///
        /// If `None`, uses the focused window.
        #[cfg_attr(feature = "clap", arg(long))]
        id: Option<u64>,
    },
    /// Switch between preset window widths backwards.
    SwitchPresetWindowWidthBack {
        /// Id of the window whose width to switch.
        ///
        /// If `None`, uses the focused window.
        #[cfg_attr(feature = "clap", arg(long))]
        id: Option<u64>,
    },
    /// Switch between preset window heights.
    SwitchPresetWindowHeight {
        /// Id of the window whose height to switch.
        ///
        /// If `None`, uses the focused window.
        #[cfg_attr(feature = "clap", arg(long))]
        id: Option<u64>,
    },
    /// Switch between preset window heights backwards.
    SwitchPresetWindowHeightBack {
        /// Id of the window whose height to switch.
        ///
        /// If `None`, uses the focused window.
        #[cfg_attr(feature = "clap", arg(long))]
        id: Option<u64>,
    },
    /// Toggle the maximized state of the focused column.
    MaximizeColumn {},
    /// Toggle the maximized-to-edges state of the focused window.
    MaximizeWindowToEdges {
        /// Id of the window to maximize.
        ///
        /// If `None`, uses the focused window.
        #[cfg_attr(feature = "clap", arg(long))]
        id: Option<u64>,
    },
    /// Change the width of the focused column.
    SetColumnWidth {
        /// How to change the width.
        #[cfg_attr(feature = "clap", arg(allow_hyphen_values = true))]
        change: SizeChange,
    },
    /// Expand the focused column to space not taken up by other fully visible columns.
    ExpandColumnToAvailableWidth {},
    /// Switch between keyboard layouts.
    SwitchLayout {
        /// Layout to switch to.
        #[cfg_attr(feature = "clap", arg())]
        layout: LayoutSwitchTarget,
    },
    /// Show the hotkey overlay.
    ShowHotkeyOverlay {},
    /// Move the focused workspace to the monitor to the left.
    MoveWorkspaceToMonitorLeft {},
    /// Move the focused workspace to the monitor to the right.
    MoveWorkspaceToMonitorRight {},
    /// Move the focused workspace to the monitor below.
    MoveWorkspaceToMonitorDown {},
    /// Move the focused workspace to the monitor above.
    MoveWorkspaceToMonitorUp {},
    /// Move the focused workspace to the previous monitor.
    MoveWorkspaceToMonitorPrevious {},
    /// Move the focused workspace to the next monitor.
    MoveWorkspaceToMonitorNext {},
    /// Move a workspace to a specific monitor.
    #[cfg_attr(
        feature = "clap",
        clap(about = "Move the focused workspace to a specific monitor")
    )]
    MoveWorkspaceToMonitor {
        /// The target output name.
        #[cfg_attr(feature = "clap", arg())]
        output: String,

        // Reference (index or name) of the workspace to move.
        ///
        /// If `None`, uses the focused workspace.
        #[cfg_attr(feature = "clap", arg(long))]
        reference: Option<WorkspaceReferenceArg>,
    },
    /// Toggle a debug tint on windows.
    ToggleDebugTint {},
    /// Toggle visualization of render element opaque regions.
    DebugToggleOpaqueRegions {},
    /// Toggle visualization of output damage.
    DebugToggleDamage {},
    /// Move the focused window between the floating and the tiling layout.
    ToggleWindowFloating {
        /// Id of the window to move.
        ///
        /// If `None`, uses the focused window.
        #[cfg_attr(feature = "clap", arg(long))]
        id: Option<u64>,
    },
    /// Move the focused window to the floating layout.
    MoveWindowToFloating {
        /// Id of the window to move.
        ///
        /// If `None`, uses the focused window.
        #[cfg_attr(feature = "clap", arg(long))]
        id: Option<u64>,
    },
    /// Move the focused window to the tiling layout.
    MoveWindowToTiling {
        /// Id of the window to move.
        ///
        /// If `None`, uses the focused window.
        #[cfg_attr(feature = "clap", arg(long))]
        id: Option<u64>,
    },
    /// Switches focus to the floating layout.
    FocusFloating {},
    /// Switches focus to the tiling layout.
    FocusTiling {},
    /// Toggles the focus between the floating and the tiling layout.
    SwitchFocusBetweenFloatingAndTiling {},
    /// Move a floating window on screen.
    #[cfg_attr(feature = "clap", clap(about = "Move the floating window on screen"))]
    MoveFloatingWindow {
        /// Id of the window to move.
        ///
        /// If `None`, uses the focused window.
        #[cfg_attr(feature = "clap", arg(long))]
        id: Option<u64>,

        /// How to change the X position.
        #[cfg_attr(
            feature = "clap",
            arg(short, long, default_value = "+0", allow_hyphen_values = true)
        )]
        x: PositionChange,

        /// How to change the Y position.
        #[cfg_attr(
            feature = "clap",
            arg(short, long, default_value = "+0", allow_hyphen_values = true)
        )]
        y: PositionChange,
    },
    /// Toggle the opacity of a window.
    #[cfg_attr(
        feature = "clap",
        clap(about = "Toggle the opacity of the focused window")
    )]
    ToggleWindowRuleOpacity {
        /// Id of the window.
        ///
        /// If `None`, uses the focused window.
        #[cfg_attr(feature = "clap", arg(long))]
        id: Option<u64>,
    },
    /// Set the dynamic cast target to a window.
    #[cfg_attr(
        feature = "clap",
        clap(about = "Set the dynamic cast target to the focused window")
    )]
    SetDynamicCastWindow {
        /// Id of the window to target.
        ///
        /// If `None`, uses the focused window.
        #[cfg_attr(feature = "clap", arg(long))]
        id: Option<u64>,
    },
    /// Set the dynamic cast target to a monitor.
    #[cfg_attr(
        feature = "clap",
        clap(about = "Set the dynamic cast target to the focused monitor")
    )]
    SetDynamicCastMonitor {
        /// Name of the output to target.
        ///
        /// If `None`, uses the focused output.
        #[cfg_attr(feature = "clap", arg())]
        output: Option<String>,
    },
    /// Clear the dynamic cast target, making it show nothing.
    ClearDynamicCastTarget {},
    /// Stop a PipeWire screencast.
    ///
    /// wlr-screencopy screencasts cannot currently be stopped via IPC.
    StopCast {
        /// Session ID of the screencast to stop.
        ///
        /// If the session has multiple screencast streams, this will stop all of them.
        #[cfg_attr(feature = "clap", arg(long))]
        session_id: u64,
    },
    /// Toggle (open/close) the Overview.
    ToggleOverview {},
    /// Open the Overview.
    OpenOverview {},
    /// Close the Overview.
    CloseOverview {},
    /// Toggle urgent status of a window.
    ToggleWindowUrgent {
        /// Id of the window to toggle urgent.
        #[cfg_attr(feature = "clap", arg(long))]
        id: u64,
    },
    /// Set urgent status of a window.
    SetWindowUrgent {
        /// Id of the window to set urgent.
        #[cfg_attr(feature = "clap", arg(long))]
        id: u64,
    },
    /// Unset urgent status of a window.
    UnsetWindowUrgent {
        /// Id of the window to unset urgent.
        #[cfg_attr(feature = "clap", arg(long))]
        id: u64,
    },
    /// Reload the config file.
    ///
    /// Can be useful for scripts changing the config file, to avoid waiting the small duration for
    /// niri's config file watcher to notice the changes.
    LoadConfigFile {
        /// Path of a new config file to load.
        ///
        /// If unset, reloads the current config file.
        #[cfg_attr(feature = "clap", arg(long))]
        path: Option<String>,
    },
}

/// Change in window or column size.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub enum SizeChange {
    /// Set the size in logical pixels.
    SetFixed(i32),
    /// Set the size as a proportion of the working area.
    SetProportion(f64),
    /// Add or subtract to the current size in logical pixels.
    AdjustFixed(i32),
    /// Add or subtract to the current size as a proportion of the working area.
    AdjustProportion(f64),
}

/// Change in floating window position.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub enum PositionChange {
    /// Set the position in logical pixels.
    SetFixed(f64),
    /// Set the position as a proportion of the working area.
    SetProportion(f64),
    /// Add or subtract to the current position in logical pixels.
    AdjustFixed(f64),
    /// Add or subtract to the current position as a proportion of the working area.
    AdjustProportion(f64),
}

/// Workspace reference (id, index or name) to operate on.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub enum WorkspaceReferenceArg {
    /// Id of the workspace.
    Id(u64),
    /// Index of the workspace.
    Index(u8),
    /// Name of the workspace.
    Name(String),
}

/// Layout to switch to.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub enum LayoutSwitchTarget {
    /// The next configured layout.
    Next,
    /// The previous configured layout.
    Prev,
    /// The specific layout by index.
    Index(u8),
}

/// How windows display in a column.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub enum ColumnDisplay {
    /// Windows are tiled vertically across the working area height.
    Normal,
    /// Windows are in tabs.
    Tabbed,
}

/// Output actions that niri can perform.
// Variants in this enum should match the spelling of the ones in niri-config. Most thigs from
// niri-config should be present here.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "clap", derive(clap::Parser))]
#[cfg_attr(feature = "clap", command(subcommand_value_name = "ACTION"))]
#[cfg_attr(feature = "clap", command(subcommand_help_heading = "Actions"))]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub enum OutputAction {
    /// Turn off the output.
    Off,
    /// Turn on the output.
    On,
    /// Set the output mode.
    Mode {
        /// Mode to set, or "auto" for automatic selection.
        ///
        /// Run `niri msg outputs` to see the available modes.
        #[cfg_attr(feature = "clap", arg())]
        mode: ModeToSet,
    },
    /// Set a custom output mode.
    CustomMode {
        /// Custom mode to set.
        #[cfg_attr(feature = "clap", arg())]
        mode: ConfiguredMode,
    },
    /// Set a custom VESA CVT modeline.
    #[cfg_attr(feature = "clap", arg())]
    Modeline {
        /// The rate at which pixels are drawn in MHz.
        #[cfg_attr(feature = "clap", arg())]
        clock: f64,
        /// Horizontal active pixels.
        #[cfg_attr(feature = "clap", arg())]
        hdisplay: u16,
        /// Horizontal sync pulse start position in pixels.
        #[cfg_attr(feature = "clap", arg())]
        hsync_start: u16,
        /// Horizontal sync pulse end position in pixels.
        #[cfg_attr(feature = "clap", arg())]
        hsync_end: u16,
        /// Total horizontal number of pixels before resetting the horizontal drawing position to
        /// zero.
        #[cfg_attr(feature = "clap", arg())]
        htotal: u16,

        /// Vertical active pixels.
        #[cfg_attr(feature = "clap", arg())]
        vdisplay: u16,
        /// Vertical sync pulse start position in pixels.
        #[cfg_attr(feature = "clap", arg())]
        vsync_start: u16,
        /// Vertical sync pulse end position in pixels.
        #[cfg_attr(feature = "clap", arg())]
        vsync_end: u16,
        /// Total vertical number of pixels before resetting the vertical drawing position to zero.
        #[cfg_attr(feature = "clap", arg())]
        vtotal: u16,
        /// Horizontal sync polarity: "+hsync" or "-hsync".
        #[cfg_attr(feature = "clap", arg(allow_hyphen_values = true))]
        hsync_polarity: HSyncPolarity,
        /// Vertical sync polarity: "+vsync" or "-vsync".
        #[cfg_attr(feature = "clap", arg(allow_hyphen_values = true))]
        vsync_polarity: VSyncPolarity,
    },
    /// Set the output scale.
    Scale {
        /// Scale factor to set, or "auto" for automatic selection.
        #[cfg_attr(feature = "clap", arg())]
        scale: ScaleToSet,
    },
    /// Set the output transform.
    Transform {
        /// Transform to set, counter-clockwise.
        #[cfg_attr(feature = "clap", arg())]
        transform: Transform,
    },
    /// Set the output position.
    Position {
        /// Position to set, or "auto" for automatic selection.
        #[cfg_attr(feature = "clap", command(subcommand))]
        position: PositionToSet,
    },
    /// Set the variable refresh rate mode.
    Vrr {
        /// Variable refresh rate mode to set.
        #[cfg_attr(feature = "clap", command(flatten))]
        vrr: VrrToSet,
    },
    /// Set the maximum bits per channel (bit depth).
    MaxBpc {
        /// Maximum bits per channel to set.
        #[cfg_attr(feature = "clap", arg())]
        max_bpc: MaxBpc,
    },
}

/// Output mode to set.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub enum ModeToSet {
    /// Niri will pick the mode automatically.
    Automatic,
    /// Specific mode.
    Specific(ConfiguredMode),
}

/// Output mode as set in the config file.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ConfiguredMode {
    /// Width in physical pixels.
    pub width: u16,
    /// Height in physical pixels.
    pub height: u16,
    /// Refresh rate.
    pub refresh: Option<f64>,
}

/// Modeline horizontal syncing polarity.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub enum HSyncPolarity {
    /// Positive polarity.
    PHSync,
    /// Negative polarity.
    NHSync,
}

/// Modeline vertical syncing polarity.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub enum VSyncPolarity {
    /// Positive polarity.
    PVSync,
    /// Negative polarity.
    NVSync,
}

/// Output scale to set.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub enum ScaleToSet {
    /// Niri will pick the scale automatically.
    Automatic,
    /// Specific scale.
    Specific(f64),
}

/// Output position to set.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "clap", derive(clap::Subcommand))]
#[cfg_attr(feature = "clap", command(subcommand_value_name = "POSITION"))]
#[cfg_attr(feature = "clap", command(subcommand_help_heading = "Position Values"))]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub enum PositionToSet {
    /// Position the output automatically.
    #[cfg_attr(feature = "clap", command(name = "auto"))]
    Automatic,
    /// Set a specific position.
    #[cfg_attr(feature = "clap", command(name = "set"))]
    Specific(ConfiguredPosition),
}

/// Output position as set in the config file.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "clap", derive(clap::Args))]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ConfiguredPosition {
    /// Logical X position.
    pub x: i32,
    /// Logical Y position.
    pub y: i32,
}

/// Output VRR to set.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "clap", derive(clap::Args))]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct VrrToSet {
    /// Whether to enable variable refresh rate.
    #[cfg_attr(
        feature = "clap",
        arg(
            value_name = "ON|OFF",
            action = clap::ArgAction::Set,
            value_parser = clap::builder::BoolishValueParser::new(),
            hide_possible_values = true,
        ),
    )]
    pub vrr: bool,
    /// Only enable when the output shows a window matching the variable-refresh-rate window rule.
    #[cfg_attr(feature = "clap", arg(long))]
    pub on_demand: bool,
}

/// Connected output.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct Output {
    /// Name of the output.
    pub name: String,
    /// Textual description of the manufacturer.
    pub make: String,
    /// Textual description of the model.
    pub model: String,
    /// Serial of the output, if known.
    pub serial: Option<String>,
    /// Physical width and height of the output in millimeters, if known.
    pub physical_size: Option<(u32, u32)>,
    /// Available modes for the output.
    pub modes: Vec<Mode>,
    /// Index of the current mode in [`Self::modes`].
    ///
    /// `None` if the output is disabled.
    pub current_mode: Option<usize>,
    /// Whether the current_mode is a custom mode.
    pub is_custom_mode: bool,
    /// Whether the output supports variable refresh rate.
    pub vrr_supported: bool,
    /// Whether variable refresh rate is enabled on the output.
    pub vrr_enabled: bool,
    /// Logical output information.
    ///
    /// `None` if the output is not mapped to any logical output (for example, if it is disabled).
    pub logical: Option<LogicalOutput>,
    /// Maximum bits per channel (bit depth), if known.
    pub max_bpc: Option<u8>,
}

/// Output mode.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct Mode {
    /// Width in physical pixels.
    pub width: u16,
    /// Height in physical pixels.
    pub height: u16,
    /// Refresh rate in millihertz.
    pub refresh_rate: u32,
    /// Whether this mode is preferred by the monitor.
    pub is_preferred: bool,
}

/// Logical output in the compositor's coordinate space.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct LogicalOutput {
    /// Logical X position.
    pub x: i32,
    /// Logical Y position.
    pub y: i32,
    /// Width in logical pixels.
    pub width: u32,
    /// Height in logical pixels.
    pub height: u32,
    /// Scale factor.
    pub scale: f64,
    /// Transform.
    pub transform: Transform,
}

/// Output transform, which goes counter-clockwise.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub enum Transform {
    /// Untransformed.
    Normal,
    /// Rotated by 90°.
    #[serde(rename = "90")]
    _90,
    /// Rotated by 180°.
    #[serde(rename = "180")]
    _180,
    /// Rotated by 270°.
    #[serde(rename = "270")]
    _270,
    /// Flipped horizontally.
    Flipped,
    /// Rotated by 90° and flipped horizontally.
    #[cfg_attr(feature = "clap", value(name("flipped-90")))]
    Flipped90,
    /// Flipped vertically.
    #[cfg_attr(feature = "clap", value(name("flipped-180")))]
    Flipped180,
    /// Rotated by 270° and flipped horizontally.
    #[cfg_attr(feature = "clap", value(name("flipped-270")))]
    Flipped270,
}

/// Output maximum bits per channel.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub enum MaxBpc {
    /// 6-bit.
    #[serde(rename = "6")]
    _6 = 6,
    /// 8-bit.
    #[default]
    #[serde(rename = "8")]
    _8 = 8,
    /// 10-bit.
    #[serde(rename = "10")]
    _10 = 10,
    /// 12-bit.
    #[serde(rename = "12")]
    _12 = 12,
    /// 14-bit.
    #[serde(rename = "14")]
    _14 = 14,
    /// 16-bit.
    #[serde(rename = "16")]
    _16 = 16,
}

/// Toplevel window.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct Window {
    /// Unique id of this window.
    ///
    /// This id remains constant while this window is open.
    ///
    /// Do not assume that window ids will always increase without wrapping, or start at 1. That is
    /// an implementation detail subject to change. For example, ids may change to be randomly
    /// generated for each new window.
    pub id: u64,
    /// Title, if set.
    pub title: Option<String>,
    /// Application ID, if set.
    pub app_id: Option<String>,
    /// Process ID that created the Wayland connection for this window, if known.
    ///
    /// Currently, windows created by xdg-desktop-portal-gnome will have a `None` PID, but this may
    /// change in the future.
    pub pid: Option<i32>,
    /// Id of the workspace this window is on, if any.
    pub workspace_id: Option<u64>,
    /// Whether this window is currently focused.
    ///
    /// There can be either one focused window or zero (e.g. when a layer-shell surface has focus).
    pub is_focused: bool,
    /// Whether this window is currently floating.
    ///
    /// If the window isn't floating then it is in the tiling layout.
    pub is_floating: bool,
    /// Whether this window requests your attention.
    pub is_urgent: bool,
    /// Position- and size-related properties of the window.
    pub layout: WindowLayout,
    /// Timestamp when the window was most recently focused.
    ///
    /// This timestamp is intended for most-recently-used window switchers, i.e. Alt-Tab. It only
    /// updates after some debounce time so that quick window switching doesn't mark intermediate
    /// windows as recently focused.
    ///
    /// The timestamp comes from the monotonic clock.
    pub focus_timestamp: Option<Timestamp>,
}

/// A moment in time.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct Timestamp {
    /// Number of whole seconds.
    pub secs: u64,
    /// Fractional part of the timestamp in nanoseconds (10<sup>-9</sup> seconds).
    pub nanos: u32,
}

/// Position- and size-related properties of a [`Window`].
///
/// Optional properties will be unset for some windows, do not rely on them being present. Whether
/// some optional properties are present or absent for certain window types may change across niri
/// releases.
///
/// All sizes and positions are in *logical pixels* unless stated otherwise. Logical sizes may be
/// fractional. For example, at 1.25 monitor scale, a 2-physical-pixel-wide window border is 1.6
/// logical pixels wide.
///
/// This struct contains positions and sizes both for full tiles ([`Self::tile_size`],
/// [`Self::tile_pos_in_workspace_view`]) and the window geometry ([`Self::window_size`],
/// [`Self::window_offset_in_tile`]). For visual displays, use the tile properties, as they
/// correspond to what the user visually considers "window". The window properties on the other
/// hand are mainly useful when you need to know the underlying Wayland window sizes, e.g. for
/// application debugging.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct WindowLayout {
    /// Location of a tiled window within a workspace: (column index, tile index in column).
    ///
    /// The indices are 1-based, i.e. the leftmost column is at index 1 and the topmost tile in a
    /// column is at index 1. This is consistent with [`Action::FocusColumn`] and
    /// [`Action::FocusWindowInColumn`].
    pub pos_in_scrolling_layout: Option<(usize, usize)>,
    /// Size of the tile this window is in, including decorations like borders.
    pub tile_size: (f64, f64),
    /// Size of the window's visual geometry itself.
    ///
    /// Does not include niri decorations like borders.
    ///
    /// Currently, Wayland toplevel windows can only be integer-sized in logical pixels, even
    /// though it doesn't necessarily align to physical pixels.
    pub window_size: (i32, i32),
    /// Tile position within the current view of the workspace.
    ///
    /// This is the same "workspace view" as in gradients' `relative-to` in the niri config.
    pub tile_pos_in_workspace_view: Option<(f64, f64)>,
    /// Location of the window's visual geometry within its tile.
    ///
    /// This includes things like border sizes. For fullscreened fixed-size windows this includes
    /// the distance from the corner of the black backdrop to the corner of the (centered) window
    /// contents.
    pub window_offset_in_tile: (f64, f64),
}

/// Output configuration change result.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub enum OutputConfigChanged {
    /// The target output was connected and the change was applied.
    Applied,
    /// The target output was not found, the change will be applied when it is connected.
    OutputWasMissing,
}

/// A workspace.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct Workspace {
    /// Unique id of this workspace.
    ///
    /// This id remains constant regardless of the workspace moving around and across monitors.
    ///
    /// Do not assume that workspace ids will always increase without wrapping, or start at 1. That
    /// is an implementation detail subject to change. For example, ids may change to be randomly
    /// generated for each new workspace.
    pub id: u64,
    /// Index of the workspace on its monitor.
    ///
    /// This is the same index you can use for requests like `niri msg action focus-workspace`.
    ///
    /// This index *will change* as you move and re-order workspace. It is merely the workspace's
    /// current position on its monitor. Workspaces on different monitors can have the same index.
    ///
    /// If you need a unique workspace id that doesn't change, see [`Self::id`].
    pub idx: u8,
    /// Optional name of the workspace.
    pub name: Option<String>,
    /// Name of the output that the workspace is on.
    ///
    /// Can be `None` if no outputs are currently connected.
    pub output: Option<String>,
    /// Whether the workspace currently has an urgent window in its output.
    pub is_urgent: bool,
    /// Whether the workspace is currently active on its output.
    ///
    /// Every output has one active workspace, the one that is currently visible on that output.
    pub is_active: bool,
    /// Whether the workspace is currently focused.
    ///
    /// There's only one focused workspace across all outputs.
    pub is_focused: bool,
    /// Id of the active window on this workspace, if any.
    pub active_window_id: Option<u64>,
}

/// Configured keyboard layouts.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct KeyboardLayouts {
    /// XKB names of the configured layouts.
    pub names: Vec<String>,
    /// Index of the currently active layout in `names`.
    pub current_idx: u8,
}

/// A layer-shell layer.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub enum Layer {
    /// The background layer.
    Background,
    /// The bottom layer.
    Bottom,
    /// The top layer.
    Top,
    /// The overlay layer.
    Overlay,
}

/// Keyboard interactivity modes for a layer-shell surface.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub enum LayerSurfaceKeyboardInteractivity {
    /// Surface cannot receive keyboard focus.
    None,
    /// Surface receives keyboard focus whenever possible.
    Exclusive,
    /// Surface receives keyboard focus on demand, e.g. when clicked.
    OnDemand,
}

/// A layer-shell surface.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct LayerSurface {
    /// Namespace provided by the layer-shell client.
    pub namespace: String,
    /// Name of the output the surface is on.
    pub output: String,
    /// Layer that the surface is on.
    pub layer: Layer,
    /// The surface's keyboard interactivity mode.
    pub keyboard_interactivity: LayerSurfaceKeyboardInteractivity,
}

/// A screencast.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct Cast {
    /// Stream ID of the screencast that uniquely identifies it.
    pub stream_id: u64,
    /// Session ID of the screencast.
    ///
    /// A session can have multiple screencast streams. Then multiple `Cast`s will have the same
    /// `session_id`. Though, usually there's only one stream per session.
    ///
    /// Do not confuse `session_id` with [`stream_id`](Self::stream_id).
    pub session_id: u64,
    /// Kind of this screencast.
    pub kind: CastKind,
    /// Target being captured.
    pub target: CastTarget,
    /// Whether this is a Dynamic Cast Target screencast.
    ///
    /// Meaning that actions like `SetDynamicCastWindow` will act on this screencast.
    ///
    /// Keep in mind that the target can change even if this is `false`.
    pub is_dynamic_target: bool,
    /// Whether the cast is currently streaming frames.
    ///
    /// This can be `false` for example when switching away to a different scene in OBS, which
    /// pauses the stream.
    pub is_active: bool,
    /// Process ID of the screencast consumer, if known.
    ///
    /// Currently, only wlr-screencopy screencasts can have a pid.
    pub pid: Option<i32>,
    /// PipeWire node ID of the screencast stream.
    ///
    /// This is `None` for wlr-screencopy casts, and also for PipeWire casts before the node is
    /// created (when the cast is just starting up).
    pub pw_node_id: Option<u32>,
}

/// Kind of screencast.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub enum CastKind {
    /// PipeWire screencast, typically via xdg-desktop-portal-gnome.
    PipeWire,
    /// wlr-screencopy protocol screencast.
    ///
    /// Tools like wf-recorder, and the xdg-desktop-portal-wlr portal.
    ///
    /// Only wlr-screencopy with damage tracking is reported here. Screencopy without damage is
    /// treated as a regular screenshot and not reported as a screencast.
    WlrScreencopy,
    /// ext-image-copy-capture protocol screencast.
    ///
    /// Tools like wayvnc, wl-mirror, and the xdg-desktop-portal-wlr portal.
    ///
    /// A client capturing both an output and the cursor creates two sessions, which are reported
    /// as two `Cast`s sharing one [`session_id`](Cast::session_id).
    ExtImageCopyCapture,
}

/// Target of a screencast.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub enum CastTarget {
    /// The target is not yet set, or was cleared.
    Nothing {},
    /// Casting an output.
    Output {
        /// Name of the screencasted output.
        name: String,
    },
    /// Casting a window.
    Window {
        /// ID of the screencasted window.
        id: u64,
    },
}

/// Number of touch-trace entries the compositor keeps in its ring buffer.
///
/// The ring is **always populated**: it is a fixed-size in-memory buffer, so
/// keeping it filled is free and works in every session. `NIRI_IPC_TOUCH_TRACE=1`
/// gates only the *live event stream*, never this buffer. See [`TouchTrace`].
pub const TOUCH_TRACE_CAPACITY: usize = 256;

/// One buffered touch-trace event.
///
/// Identical to the [`Event`] that was also pushed on the event stream, with
/// the metadata a late-attaching client cannot otherwise recover: where it
/// falls in the sequence, and when it happened.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct TouchTraceEntry {
    /// Monotonically increasing, starts at 1, never reused.
    ///
    /// Pass this back as [`Request::TouchTrace::since`] to poll incrementally.
    pub sequence: u64,
    /// When the event was published, as [`Timestamp`].
    pub timestamp: Timestamp,
    /// The touch event itself.
    pub event: Event,
}

/// Buffered touch events, oldest first.
///
/// The compositor keeps at most [`TOUCH_TRACE_CAPACITY`] of these in a ring
/// buffer. This is current state plus a short recent history, deliberately
/// *not* full replay: the interesting failure has already happened by the time
/// a debugging tool is attached, so the last few hundred events are what
/// explain it, and an unbounded log would be an unbounded memory cost on a
/// path that runs on every finger-down.
///
/// **The ring is populated unconditionally.** It does not depend on
/// `NIRI_IPC_TOUCH_TRACE` or on any per-bind `emit-progress` flag: an empty
/// response means no touch activity has happened since the compositor started,
/// nothing else. That env var gates only the live event stream, because the
/// stream can flood a real subscriber with one line per finger-down; the ring
/// has no subscriber to flood and nobody reads it unless a client asks.
///
/// The consequence is the point of this type: a debugging tool can recover
/// evidence from a session that was never configured for tracing, which is the
/// only session whose evidence anyone ever wants. The evidence does not survive
/// a compositor restart, since the buffer is in-memory.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct TouchTrace {
    /// Entries newer than the requested `since`, oldest first.
    ///
    /// This is capped at [`TOUCH_TRACE_CAPACITY`] entries.
    pub entries: Vec<TouchTraceEntry>,
    /// The highest sequence number the compositor has issued.
    ///
    /// A client polling with `since` set to this value gets only events that
    /// happened after this response. Note that the buffer may have wrapped in
    /// the meantime, in which case events older than this have been dropped.
    pub next_sequence: u64,
}

/// Who owns a touch contact, as decided by the compositor's arbitration.
///
/// Only `Compositor` contacts reach the gesture recognizer; `Client` contacts
/// are forwarded verbatim to the focused surface; `Ignored` contacts go
/// nowhere at all.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub enum TouchOwnership {
    /// The compositor owns the contact and feeds it to the gesture recognizer.
    Compositor,
    /// The client owns the contact and receives it as a normal touch.
    Client,
    /// The contact is inert: neither the compositor nor the client sees it.
    Ignored,
}

/// A compositor event.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub enum Event {
    /// The workspace configuration has changed.
    WorkspacesChanged {
        /// The new workspace configuration.
        ///
        /// This configuration completely replaces the previous configuration. I.e. if any
        /// workspaces are missing from here, then they were deleted.
        workspaces: Vec<Workspace>,
    },
    /// The workspace urgency changed.
    WorkspaceUrgencyChanged {
        /// Id of the workspace.
        id: u64,
        /// Whether this workspace has an urgent window.
        urgent: bool,
    },
    /// A workspace was activated on an output.
    ///
    /// This doesn't always mean the workspace became focused, just that it's now the active
    /// workspace on its output. All other workspaces on the same output become inactive.
    WorkspaceActivated {
        /// Id of the newly active workspace.
        id: u64,
        /// Whether this workspace also became focused.
        ///
        /// If `true`, this is now the single focused workspace. All other workspaces are no longer
        /// focused, but they may remain active on their respective outputs.
        focused: bool,
    },
    /// An active window changed on a workspace.
    WorkspaceActiveWindowChanged {
        /// Id of the workspace on which the active window changed.
        workspace_id: u64,
        /// Id of the new active window, if any.
        active_window_id: Option<u64>,
    },
    /// The window configuration has changed.
    WindowsChanged {
        /// The new window configuration.
        ///
        /// This configuration completely replaces the previous configuration. I.e. if any windows
        /// are missing from here, then they were closed.
        windows: Vec<Window>,
    },
    /// A new toplevel window was opened, or an existing toplevel window changed.
    WindowOpenedOrChanged {
        /// The new or updated window.
        ///
        /// If the window is focused, all other windows are no longer focused.
        window: Window,
    },
    /// A toplevel window was closed.
    WindowClosed {
        /// Id of the removed window.
        id: u64,
    },
    /// Window focus changed.
    ///
    /// All other windows are no longer focused.
    WindowFocusChanged {
        /// Id of the newly focused window, or `None` if no window is now focused.
        id: Option<u64>,
    },
    /// Window focus timestamp changed.
    ///
    /// This event is separate from [`Event::WindowFocusChanged`] because the focus timestamp only
    /// updates after some debounce time so that quick window switching doesn't mark intermediate
    /// windows as recently focused.
    WindowFocusTimestampChanged {
        /// Id of the window.
        id: u64,
        /// The new focus timestamp.
        focus_timestamp: Option<Timestamp>,
    },
    /// Window urgency changed.
    WindowUrgencyChanged {
        /// Id of the window.
        id: u64,
        /// The new urgency state of the window.
        urgent: bool,
    },
    /// The layout of one or more windows has changed.
    WindowLayoutsChanged {
        /// Pairs consisting of a window id and new layout information for the window.
        changes: Vec<(u64, WindowLayout)>,
    },
    /// The configured keyboard layouts have changed.
    KeyboardLayoutsChanged {
        /// The new keyboard layout configuration.
        keyboard_layouts: KeyboardLayouts,
    },
    /// The keyboard layout switched.
    KeyboardLayoutSwitched {
        /// Index of the newly active layout.
        idx: u8,
    },
    /// The overview was opened or closed.
    OverviewOpenedOrClosed {
        /// The new state of the overview.
        is_open: bool,
    },
    /// The configuration was reloaded.
    ///
    /// You will always receive this event when connecting to the event stream, indicating the last
    /// config load attempt.
    ConfigLoaded {
        /// Whether the loading failed.
        ///
        /// For example, the config file couldn't be parsed.
        failed: bool,
    },
    /// A screenshot was captured.
    ScreenshotCaptured {
        /// The file path where the screenshot was saved, if it was written to disk.
        ///
        /// If `None`, the screenshot was either only copied to the clipboard, or the path couldn't
        /// be converted to a `String` (e.g. contained invalid UTF-8 bytes).
        path: Option<String>,
    },
    /// The screencasts have changed.
    CastsChanged {
        /// The new screencast information.
        ///
        /// This configuration completely replaces the previous configuration. I.e. if any casts
        /// are missing from here, then they were stopped.
        casts: Vec<Cast>,
    },
    /// A screencast started, or an existing cast changed.
    CastStartedOrChanged {
        /// The cast that started or changed.
        cast: Cast,
    },
    /// A screencast stopped.
    CastStopped {
        /// Stream ID of the stopped screencast.
        stream_id: u64,
    },
    /// A touchscreen gesture was recognised and has begun.
    ///
    /// Only sent for binds configured with `emit-progress true`. This event
    /// does not fire the bind; it only announces that the gesture is live.
    TouchGestureBegin {
        /// The config trigger the gesture matched, e.g. `TouchSwipe`.
        ///
        /// This is the trigger of the bind carrying `emit-progress true`
        /// that the gesture resolved to, which is what ties the begin event
        /// to the updates and the end event that follow it.
        trigger: String,
        /// Number of contacts driving the gesture.
        finger_count: u8,
        /// Displacement of the gesture along its locked axis, at the moment
        /// the gesture was recognised, in the gesture's own unit.
        ///
        /// See [`TouchGestureUpdate::displacement`] for the per-kind units;
        /// the value here is the recognizer's own reading at begin time, so
        /// it is typically small but not necessarily zero.
        displacement: f64,
    },
    /// A touchscreen gesture advanced.
    ///
    /// Only sent for binds configured with `emit-progress true`. Sent once
    /// per input frame in which the gesture's progress changed.
    TouchGestureUpdate {
        /// The config trigger the gesture matched, same as on
        /// [`TouchGestureBegin::trigger`].
        trigger: String,
        /// Change in `progress` since the previous update.
        ///
        /// The first update of a gesture measures from the begin event, where
        /// progress was already whatever the recognizer reported, so this is
        /// `progress - progress_at_begin` for the first update and
        /// `progress - previous_progress` thereafter. May be negative: the
        /// user can pull the gesture back.
        progress_delta: f64,
        /// Normalised completion of the gesture, in `0.0..=1.0`.
        ///
        /// `1.0` is the point at which the gesture has crossed the
        /// threshold that fires its bind, so a bind's action runs as soon as
        /// progress reaches `1.0`. An overlay that draws a threshold marker
        /// should place it at the right-hand end of the track.
        progress: f64,
        /// Rate of change of `progress`, in units of progress per second.
        ///
        /// Computed from the compositor's frame timestamps, so it is noisy
        /// at high frame rates; an overlay should smooth it rather than plot
        /// it raw.
        velocity: f64,
        /// Displacement of the gesture along its locked axis, in the
        /// gesture's own unit.
        ///
        /// The field is deliberately NOT named for a unit, because it does not
        /// have one: the recognizer measures each kind in whatever unit that
        /// kind moves in, and this field passes that number through unchanged
        /// rather than converting it.
        ///
        /// - `swipe`, `edge-swipe`, `corner-swipe`: **pixels** travelled along the locked axis.
        ///   Signed for `swipe` (positive means travelling in the locked direction), non-negative
        ///   for the edge and corner variants.
        /// - `pinch`: **pixels** of spread change; positive is divergence.
        /// - `rotate`: **radians** of rotation, always non-negative. This is the one kind that is
        ///   not measured in pixels, so a consumer must not plot it on a pixel-valued track.
        /// - `tap` and `tap-hold`: `0.0`. Neither has a continuous extent, so the recognizer
        ///   reports no displacement for them — the same reason their `progress` is reported as
        ///   `1.0`.
        ///
        /// Use it for the raw gesture extent an overlay draws; `progress` is the
        /// normalised form of the same quantity. Because the unit varies by
        /// kind, a consumer must know the gesture kind before plotting this —
        /// the kind is recoverable from the `trigger` string.
        displacement: f64,
    },
    /// A touchscreen gesture finished.
    ///
    /// Only sent for binds configured with `emit-progress true`. Always the
    /// last event of a gesture, whether or not it was committed.
    TouchGestureEnd {
        /// The config trigger the gesture matched, same as on
        /// [`TouchGestureBegin::trigger`].
        trigger: String,
        /// Final normalised completion, in `0.0..=1.0`.
        progress: f64,
        /// Whether the gesture reached its threshold and fired its bind.
        ///
        /// This is exactly the flag the gesture recognizer reports as
        /// `completed`: a gesture the user abandoned below threshold, or one
        /// the recognizer cancelled, reports `false` and fires nothing.
        committed: bool,
        /// Final displacement of the gesture along its locked axis, in the
        /// gesture's own unit.
        ///
        /// Same per-kind units as [`TouchGestureUpdate::displacement`],
        /// including `0.0` for `tap` and `tap-hold`.
        displacement: f64,
    },
    /// One touch-ownership decision was made.
    ///
    /// This is the *why* behind the gesture events: the compositor decides,
    /// on every finger-down, whether that finger belongs to the compositor or
    /// to the client, and only fingers it keeps ever reach the gesture
    /// recognizer. A gesture that never appears on the stream is usually a
    /// decision you want to read back here.
    ///
    /// Only sent when the compositor was started with
    /// `NIRI_IPC_TOUCH_TRACE=1`, because it fires on every touch contact
    /// rather than only on gestures.
    ///
    /// # Reading a trace
    ///
    /// Three situations account for nearly every touchscreen bug, and each
    /// has its own shape in this stream:
    ///
    /// - **The gesture never began, and no single decision looks wrong.** Read `arming_eligible`
    ///   across the whole sequence: it stops below `min_gesture_fingers`. Something in the
    ///   sequence is being excluded — a pen tip, an exclusion rect, a passthrough window — and
    ///   `finger_count` being larger than `arming_eligible` is the tell.
    /// - **A decision armed the compositor (`rule: "8_arming"`) and the client then stopped
    ///   responding to touch.** That decision's `open_client_touches` counts the contacts the
    ///   client still believes are down; they must have been closed at the seat. If they were not,
    ///   the client is holding phantom fingers and no later touch will reach it. This is the
    ///   failure the trace exists to make visible.
    /// - **A slot never gets retired.** A `TouchSlotRetired` for every `TouchArbitrationDecision`
    ///   is a healthy sequence end. A decision with no retirement means the ledger still counts a
    ///   finger that lifted, and it keeps inflating `arming_eligible` for every later contact —
    ///   which is how a phantom gesture arms out of nowhere. A `TouchSequenceReset` is the escape
    ///   hatch: it wipes the inflated tally, so a trace that goes wrong and then fixes itself on
    ///   the next action is the signature of a stale slot.
    TouchArbitrationDecision {
        /// Backend slot id of the contact this decision is about.
        ///
        /// The same id that appears on [`TouchSlotRetired::slot`]; a decision
        /// with no matching retirement is a contact the compositor never saw
        /// lift.
        slot: u32,
        /// Who owns the contact.
        ownership: TouchOwnership,
        /// Which precedence rule decided it.
        ///
        /// This is the same string niri logs as `rule=` at debug level, taken
        /// from the same function, so the log and the event can never
        /// disagree. `"8_arming"` means this contact is the one that armed
        /// the compositor; `"2_pen_draws"` means it was a pen; `"9_sequence_ownership_client"`
        /// means it joined a sequence the client already owned and stayed
        /// with the client.
        rule: String,
        /// Contacts in this sequence at decision time, including this one.
        ///
        /// The sequence is the ledger's unit, so a pen that drew earlier in
        /// the same touch sequence is counted here even though it does not
        /// count toward arming. That difference between this and
        /// `arming_eligible` is usually the explanation for a gesture that
        /// did not fire.
        finger_count: u32,
        /// Contacts in this sequence that count toward arming a gesture.
        ///
        /// Pen tips, contacts inside an exclusion rect, and contacts on a
        /// passthrough window never count. Compare against
        /// `min_gesture_fingers` to see how close this sequence came to
        /// arming.
        arming_eligible: u32,
        /// Client-visible touches the ledger held at decision time.
        ///
        /// These are contacts already forwarded to the client and still
        /// believed down by it. A non-zero value on a `Compositor` decision
        /// is the interesting case: those contacts have just been taken away
        /// from the client and must have been closed at the seat, or the
        /// client keeps a phantom finger down and stops responding to the
        /// user.
        open_client_touches: u32,
        /// Whether an earlier contact in this sequence already belonged to
        /// the compositor.
        sequence_compositor_owned: bool,
        /// Whether the contact came from a pen.
        ///
        /// A pen normally draws (`2_pen_draws`) unless Mod is held, in which
        /// case it is a gesture input like any other (`4_mod_is_the_escape_hatch`).
        is_pen: bool,
        /// Global logical x of the contact that produced this decision.
        ///
        /// This is the position at which the contact landed and at which
        /// the decision was made, in GLOBAL LOGICAL PIXELS — the same
        /// coordinate space as every other position niri reports, so an
        /// overlay can place it against the output layout directly. It is
        /// NOT relative to the output the contact landed on.
        ///
        /// This is a per-down value, not a live-updating position: it is the
        /// position at the moment of the decision and never changes
        /// afterwards. To follow a contact as it moves, read the gesture
        /// events instead.
        pos_x: f64,
        /// Global logical y of the contact that produced this decision.
        ///
        /// See [`TouchArbitrationDecision::pos_x`] for the coordinate space
        /// and for why this is a per-down value rather than a live one.
        pos_y: f64,
    },
    /// A contact was retired from the touch ledger.
    ///
    /// Emitted both on an ordinary lift and on a lift the ledger did not
    /// recognise. Only sent when the compositor was started with
    /// `NIRI_IPC_TOUCH_TRACE=1`.
    TouchSlotRetired {
        /// Backend slot id that was retired.
        slot: u32,
        /// Whether the ledger actually still held the slot.
        ///
        /// `false` means the lift arrived for a contact the ledger had
        /// already forgotten, which is the signature of a stale ledger or an
        /// out-of-order event pair.
        was_tracked: bool,
    },
    /// The touch ledger was cleared wholesale.
    ///
    /// Fired for the paths that end contacts without naming them: a yanked
    /// touch device, the last touch device going away, the seat-wide touch
    /// cancel, the global cancel at the start of every action, and output
    /// reconfiguration. Everything the ledger knew is gone at this point, so
    /// no further retirements will arrive for the slots it held. Only sent
    /// when the compositor was started with `NIRI_IPC_TOUCH_TRACE=1`.
    TouchSequenceReset {
        /// Client-visible touches the ledger held immediately before the
        /// clear.
        ///
        /// A client still believes these are down. This is normal for the
        /// global cancel that runs before every action; combined with a
        /// gesture that then never completes, it is the thing to look at.
        open_client_touches: u32,
        /// Contacts the compositor believed were on the glass immediately
        /// before the clear.
        live_contacts: u32,
    },
}

impl From<Duration> for Timestamp {
    fn from(value: Duration) -> Self {
        Timestamp {
            secs: value.as_secs(),
            nanos: value.subsec_nanos(),
        }
    }
}

impl From<Timestamp> for Duration {
    fn from(value: Timestamp) -> Self {
        Duration::new(value.secs, value.nanos)
    }
}

impl FromStr for WorkspaceReferenceArg {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let reference = if let Ok(index) = s.parse::<i32>() {
            if let Ok(idx) = u8::try_from(index) {
                Self::Index(idx)
            } else {
                return Err("workspace index must be between 0 and 255");
            }
        } else {
            Self::Name(s.to_string())
        };

        Ok(reference)
    }
}

impl FromStr for SizeChange {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.split_once('%') {
            Some((value, empty)) => {
                if !empty.is_empty() {
                    return Err("trailing characters after '%' are not allowed");
                }

                match value.bytes().next() {
                    Some(b'-' | b'+') => {
                        let value = value.parse().map_err(|_| "error parsing value")?;
                        Ok(Self::AdjustProportion(value))
                    }
                    Some(_) => {
                        let value = value.parse().map_err(|_| "error parsing value")?;
                        Ok(Self::SetProportion(value))
                    }
                    None => Err("value is missing"),
                }
            }
            None => {
                let value = s;
                match value.bytes().next() {
                    Some(b'-' | b'+') => {
                        let value = value.parse().map_err(|_| "error parsing value")?;
                        Ok(Self::AdjustFixed(value))
                    }
                    Some(_) => {
                        let value = value.parse().map_err(|_| "error parsing value")?;
                        Ok(Self::SetFixed(value))
                    }
                    None => Err("value is missing"),
                }
            }
        }
    }
}

impl FromStr for PositionChange {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.split_once('%') {
            Some((value, empty)) => {
                if !empty.is_empty() {
                    return Err("trailing characters after '%' are not allowed");
                }

                match value.bytes().next() {
                    Some(b'-' | b'+') => {
                        let value = value.parse().map_err(|_| "error parsing value")?;
                        Ok(Self::AdjustProportion(value))
                    }
                    Some(_) => {
                        let value = value.parse().map_err(|_| "error parsing value")?;
                        Ok(Self::SetProportion(value))
                    }
                    None => Err("value is missing"),
                }
            }
            None => {
                let value = s;
                match value.bytes().next() {
                    Some(b'-' | b'+') => {
                        let value = value.parse().map_err(|_| "error parsing value")?;
                        Ok(Self::AdjustFixed(value))
                    }
                    Some(_) => {
                        let value = value.parse().map_err(|_| "error parsing value")?;
                        Ok(Self::SetFixed(value))
                    }
                    None => Err("value is missing"),
                }
            }
        }
    }
}

impl FromStr for LayoutSwitchTarget {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "next" => Ok(Self::Next),
            "prev" => Ok(Self::Prev),
            other => match other.parse() {
                Ok(layout) => Ok(Self::Index(layout)),
                _ => Err(r#"invalid layout action, can be "next", "prev" or a layout index"#),
            },
        }
    }
}

impl FromStr for ColumnDisplay {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "normal" => Ok(Self::Normal),
            "tabbed" => Ok(Self::Tabbed),
            _ => Err(r#"invalid column display, can be "normal" or "tabbed""#),
        }
    }
}

impl FromStr for Transform {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "normal" => Ok(Self::Normal),
            "90" => Ok(Self::_90),
            "180" => Ok(Self::_180),
            "270" => Ok(Self::_270),
            "flipped" => Ok(Self::Flipped),
            "flipped-90" => Ok(Self::Flipped90),
            "flipped-180" => Ok(Self::Flipped180),
            "flipped-270" => Ok(Self::Flipped270),
            _ => Err(concat!(
                r#"invalid transform, can be "90", "180", "270", "#,
                r#""flipped", "flipped-90", "flipped-180" or "flipped-270""#
            )),
        }
    }
}

impl TryFrom<u8> for MaxBpc {
    type Error = &'static str;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            6 => Ok(MaxBpc::_6),
            8 => Ok(MaxBpc::_8),
            10 => Ok(MaxBpc::_10),
            12 => Ok(MaxBpc::_12),
            14 => Ok(MaxBpc::_14),
            16 => Ok(MaxBpc::_16),
            _ => Err("invalid max-bpc, can be 6, 8, 10, 12, 14, 16"),
        }
    }
}

impl FromStr for MaxBpc {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::try_from(s.parse::<u8>().unwrap_or_default())
    }
}

impl FromStr for Layer {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "background" => Ok(Self::Background),
            "bottom" => Ok(Self::Bottom),
            "top" => Ok(Self::Top),
            "overlay" => Ok(Self::Overlay),
            _ => Err("invalid layer, can be \"background\", \"bottom\", \"top\" or \"overlay\""),
        }
    }
}

impl FromStr for ModeToSet {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.eq_ignore_ascii_case("auto") {
            return Ok(Self::Automatic);
        }

        let mode = s.parse()?;
        Ok(Self::Specific(mode))
    }
}

impl FromStr for ConfiguredMode {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let Some((width, rest)) = s.split_once('x') else {
            return Err("no 'x' separator found");
        };

        let (height, refresh) = match rest.split_once('@') {
            Some((height, refresh)) => (height, Some(refresh)),
            None => (rest, None),
        };

        let width = width.parse().map_err(|_| "error parsing width")?;
        let height = height.parse().map_err(|_| "error parsing height")?;
        let refresh = refresh
            .map(str::parse)
            .transpose()
            .map_err(|_| "error parsing refresh rate")?;

        Ok(Self {
            width,
            height,
            refresh,
        })
    }
}

impl FromStr for HSyncPolarity {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "+hsync" => Ok(Self::PHSync),
            "-hsync" => Ok(Self::NHSync),
            _ => Err(r#"invalid horizontal sync polarity, can be "+hsync" or "-hsync"#),
        }
    }
}

impl FromStr for VSyncPolarity {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "+vsync" => Ok(Self::PVSync),
            "-vsync" => Ok(Self::NVSync),
            _ => Err(r#"invalid vertical sync polarity, can be "+vsync" or "-vsync"#),
        }
    }
}

impl FromStr for ScaleToSet {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.eq_ignore_ascii_case("auto") {
            return Ok(Self::Automatic);
        }

        let scale = s.parse().map_err(|_| "error parsing scale")?;
        Ok(Self::Specific(scale))
    }
}

macro_rules! ensure {
    ($cond:expr, $fmt:literal $($arg:tt)* ) => {
        if !$cond {
            return Err(format!($fmt $($arg)*));
        }
    };
}

impl OutputAction {
    /// Validates some required constraints on the modeline and custom mode.
    pub fn validate(&self) -> Result<(), String> {
        match self {
            OutputAction::Modeline {
                hdisplay,
                hsync_start,
                hsync_end,
                htotal,
                vdisplay,
                vsync_start,
                vsync_end,
                vtotal,
                ..
            } => {
                ensure!(
                    hdisplay < hsync_start,
                    "hdisplay {} must be < hsync_start {}",
                    hdisplay,
                    hsync_start
                );
                ensure!(
                    hsync_start < hsync_end,
                    "hsync_start {} must be < hsync_end {}",
                    hsync_start,
                    hsync_end
                );
                ensure!(
                    hsync_end < htotal,
                    "hsync_end {} must be < htotal {}",
                    hsync_end,
                    htotal
                );
                ensure!(0 < *htotal, "htotal {} must be > 0", htotal);
                ensure!(
                    vdisplay < vsync_start,
                    "vdisplay {} must be < vsync_start {}",
                    vdisplay,
                    vsync_start
                );
                ensure!(
                    vsync_start < vsync_end,
                    "vsync_start {} must be < vsync_end {}",
                    vsync_start,
                    vsync_end
                );
                ensure!(
                    vsync_end < vtotal,
                    "vsync_end {} must be < vtotal {}",
                    vsync_end,
                    vtotal
                );
                ensure!(0 < *vtotal, "vtotal {} must be > 0", vtotal);
                Ok(())
            }
            OutputAction::CustomMode {
                mode: ConfiguredMode { refresh, .. },
            } => {
                if refresh.is_none() {
                    return Err("refresh rate is required for custom modes".to_string());
                }
                if let Some(refresh) = refresh {
                    if *refresh <= 0. {
                        return Err(format!("custom mode refresh rate {refresh} must be > 0"));
                    }
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_size_change() {
        assert_eq!(
            "10".parse::<SizeChange>().unwrap(),
            SizeChange::SetFixed(10),
        );
        assert_eq!(
            "+10".parse::<SizeChange>().unwrap(),
            SizeChange::AdjustFixed(10),
        );
        assert_eq!(
            "-10".parse::<SizeChange>().unwrap(),
            SizeChange::AdjustFixed(-10),
        );
        assert_eq!(
            "10%".parse::<SizeChange>().unwrap(),
            SizeChange::SetProportion(10.),
        );
        assert_eq!(
            "+10%".parse::<SizeChange>().unwrap(),
            SizeChange::AdjustProportion(10.),
        );
        assert_eq!(
            "-10%".parse::<SizeChange>().unwrap(),
            SizeChange::AdjustProportion(-10.),
        );

        assert!("-".parse::<SizeChange>().is_err());
        assert!("10% ".parse::<SizeChange>().is_err());
    }

    #[test]
    fn parse_position_change() {
        assert_eq!(
            "10".parse::<PositionChange>().unwrap(),
            PositionChange::SetFixed(10.),
        );
        assert_eq!(
            "+10".parse::<PositionChange>().unwrap(),
            PositionChange::AdjustFixed(10.),
        );
        assert_eq!(
            "-10".parse::<PositionChange>().unwrap(),
            PositionChange::AdjustFixed(-10.),
        );

        assert_eq!(
            "10%".parse::<PositionChange>().unwrap(),
            PositionChange::SetProportion(10.)
        );
        assert_eq!(
            "+10%".parse::<PositionChange>().unwrap(),
            PositionChange::AdjustProportion(10.)
        );
        assert_eq!(
            "-10%".parse::<PositionChange>().unwrap(),
            PositionChange::AdjustProportion(-10.)
        );
        assert!("-".parse::<PositionChange>().is_err());
        assert!("10% ".parse::<PositionChange>().is_err());
    }

    /// Pins the wire format of the gesture-progress events.
    ///
    /// An external overlay is written against these exact JSON strings, so
    /// the expected output is spelled out literally rather than derived from
    /// the types: renaming a field or a variant has to fail here.
    #[test]
    fn touch_gesture_event_json_round_trip() {
        let events = [
            (
                Event::TouchGestureBegin {
                    trigger: "TouchSwipe fingers=3 direction=\"left\"".to_string(),
                    finger_count: 3,
                    displacement: 12.5,
                },
                r#"{"TouchGestureBegin":{"trigger":"TouchSwipe fingers=3 direction=\"left\"","finger_count":3,"displacement":12.5}}"#,
            ),
            (
                Event::TouchGestureUpdate {
                    trigger: "TouchSwipe fingers=3 direction=\"left\"".to_string(),
                    progress_delta: 0.25,
                    progress: 0.75,
                    velocity: 1.5,
                    displacement: 140.0,
                },
                r#"{"TouchGestureUpdate":{"trigger":"TouchSwipe fingers=3 direction=\"left\"","progress_delta":0.25,"progress":0.75,"velocity":1.5,"displacement":140.0}}"#,
            ),
            (
                Event::TouchGestureEnd {
                    trigger: "TouchSwipe fingers=3 direction=\"left\"".to_string(),
                    progress: 1.0,
                    committed: true,
                    displacement: 187.5,
                },
                r#"{"TouchGestureEnd":{"trigger":"TouchSwipe fingers=3 direction=\"left\"","progress":1.0,"committed":true,"displacement":187.5}}"#,
            ),
        ];

        for (event, expected) in events {
            let json = serde_json::to_string(&event).unwrap();
            assert_eq!(json, expected);

            // And back again, so a client that speaks this JSON is served by
            // the same enum rather than by a lookalike.
            let parsed: Event = serde_json::from_str(&json).unwrap();
            match (&event, &parsed) {
                (
                    Event::TouchGestureBegin {
                        trigger,
                        finger_count,
                        displacement,
                    },
                    Event::TouchGestureBegin {
                        trigger: trigger2,
                        finger_count: finger_count2,
                        displacement: displacement2,
                    },
                ) => {
                    assert_eq!(trigger, trigger2);
                    assert_eq!(finger_count, finger_count2);
                    assert_eq!(displacement, displacement2);
                    assert!(*displacement > 0.0, "begin carries a non-zero displacement");
                }
                (
                    Event::TouchGestureUpdate {
                        trigger,
                        progress_delta,
                        progress,
                        velocity,
                        displacement,
                    },
                    Event::TouchGestureUpdate {
                        trigger: trigger2,
                        progress_delta: progress_delta2,
                        progress: progress2,
                        velocity: velocity2,
                        displacement: displacement2,
                    },
                ) => {
                    assert_eq!(trigger, trigger2);
                    assert_eq!(progress_delta, progress_delta2);
                    assert_eq!(progress, progress2);
                    assert_eq!(velocity, velocity2);
                    assert_eq!(displacement, displacement2);
                    assert!(
                        *displacement > 0.0,
                        "update carries a non-zero displacement"
                    );
                }
                (
                    Event::TouchGestureEnd {
                        trigger,
                        progress,
                        committed,
                        displacement,
                    },
                    Event::TouchGestureEnd {
                        trigger: trigger2,
                        progress: progress2,
                        committed: committed2,
                        displacement: displacement2,
                    },
                ) => {
                    assert_eq!(trigger, trigger2);
                    assert_eq!(progress, progress2);
                    assert_eq!(committed, committed2);
                    assert_eq!(displacement, displacement2);
                    assert!(*displacement > 0.0, "end carries a non-zero displacement");
                }
                _ => panic!("round-tripped into a different variant"),
            }
        }
    }

    /// Pins the wire format of the touch-arbitration trace, including the
    /// rule-name field: it is a free-form string today, and this is what
    /// keeps the compositor and a 2am debugging session agreeing on it.
    #[test]
    fn touch_arbitration_event_json_round_trip() {
        let events = [
            (
                Event::TouchArbitrationDecision {
                    slot: 7,
                    ownership: TouchOwnership::Compositor,
                    rule: "8_arming".to_string(),
                    finger_count: 3,
                    arming_eligible: 2,
                    open_client_touches: 2,
                    sequence_compositor_owned: false,
                    is_pen: false,
                    pos_x: 1234.5,
                    pos_y: -678.25,
                },
                r#"{"TouchArbitrationDecision":{"slot":7,"ownership":"Compositor","rule":"8_arming","finger_count":3,"arming_eligible":2,"open_client_touches":2,"sequence_compositor_owned":false,"is_pen":false,"pos_x":1234.5,"pos_y":-678.25}}"#,
            ),
            (
                Event::TouchArbitrationDecision {
                    slot: 0,
                    ownership: TouchOwnership::Client,
                    rule: "2_pen_draws".to_string(),
                    finger_count: 1,
                    arming_eligible: 0,
                    open_client_touches: 1,
                    sequence_compositor_owned: false,
                    is_pen: true,
                    pos_x: 0.0,
                    pos_y: 0.0,
                },
                r#"{"TouchArbitrationDecision":{"slot":0,"ownership":"Client","rule":"2_pen_draws","finger_count":1,"arming_eligible":0,"open_client_touches":1,"sequence_compositor_owned":false,"is_pen":true,"pos_x":0.0,"pos_y":0.0}}"#,
            ),
            (
                Event::TouchSlotRetired {
                    slot: 7,
                    was_tracked: true,
                },
                r#"{"TouchSlotRetired":{"slot":7,"was_tracked":true}}"#,
            ),
            (
                Event::TouchSequenceReset {
                    open_client_touches: 2,
                    live_contacts: 3,
                },
                r#"{"TouchSequenceReset":{"open_client_touches":2,"live_contacts":3}}"#,
            ),
        ];

        for (event, expected) in events {
            let json = serde_json::to_string(&event).unwrap();
            assert_eq!(json, expected);

            let parsed: Event = serde_json::from_str(&json).unwrap();
            assert_eq!(
                serde_json::to_string(&parsed).unwrap(),
                expected,
                "{event:?} did not survive a round trip"
            );
        }
    }

    /// Pins the wire format of the touch-trace read-back request.
    ///
    /// A late-attaching debugging client is written against these exact bytes,
    /// so the request is spelled out as literal JSON the way the event tests
    /// spell out their events.
    #[test]
    fn touch_trace_request_json_round_trip() {
        for (json, expected) in [
            (
                r#"{"TouchTrace":{"since":0}}"#,
                Request::TouchTrace { since: 0 },
            ),
            (
                r#"{"TouchTrace":{"since":42}}"#,
                Request::TouchTrace { since: 42 },
            ),
        ] {
            let parsed: Request = serde_json::from_str(json).unwrap();
            match (&parsed, &expected) {
                (Request::TouchTrace { since }, Request::TouchTrace { since: since2 }) => {
                    assert_eq!(since, since2);
                }
                _ => panic!("parsed into a different variant"),
            }
            assert_eq!(serde_json::to_string(&parsed).unwrap(), json);
        }
    }

    /// Pins the wire format of the touch-trace read-back response, including
    /// the empty-ring case a client hits before anything has happened.
    #[test]
    fn touch_trace_response_json_round_trip() {
        let responses = [
            (
                Response::TouchTrace(TouchTrace {
                    entries: Vec::new(),
                    next_sequence: 1,
                }),
                r#"{"TouchTrace":{"entries":[],"next_sequence":1}}"#,
            ),
            (
                Response::TouchTrace(TouchTrace {
                    entries: vec![TouchTraceEntry {
                        sequence: 7,
                        timestamp: Timestamp {
                            secs: 3,
                            nanos: 500_000_000,
                        },
                        event: Event::TouchArbitrationDecision {
                            slot: 7,
                            ownership: TouchOwnership::Compositor,
                            rule: "8_arming".to_string(),
                            finger_count: 3,
                            arming_eligible: 2,
                            open_client_touches: 2,
                            sequence_compositor_owned: false,
                            is_pen: false,
                            pos_x: 1234.5,
                            pos_y: -678.25,
                        },
                    }],
                    next_sequence: 8,
                }),
                concat!(
                    r#"{"TouchTrace":{"entries":[{"sequence":7,"#,
                    r#""timestamp":{"secs":3,"nanos":500000000},"#,
                    r#""event":{"TouchArbitrationDecision":{"slot":7,"#,
                    r#""ownership":"Compositor","rule":"8_arming","#,
                    r#""finger_count":3,"arming_eligible":2,"#,
                    r#""open_client_touches":2,"sequence_compositor_owned":false,"#,
                    r#""is_pen":false,"pos_x":1234.5,"pos_y":-678.25}}}],"next_sequence":8}}"#,
                ),
            ),
        ];

        for (response, expected) in responses {
            let json = serde_json::to_string(&response).unwrap();
            assert_eq!(json, expected);

            let parsed: Response = serde_json::from_str(&json).unwrap();
            assert_eq!(
                serde_json::to_string(&parsed).unwrap(),
                expected,
                "{response:?} did not survive a round trip"
            );
        }
    }
}

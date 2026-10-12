### Overview

In this section you can configure input devices like keyboard and mouse, and some input-related options.

There's a section for each device type: `keyboard`, `touchpad`, `mouse`, `trackpoint`, `trackball`, `tablet`, `touchscreen`.
Settings in those sections will apply to every device of that type.
Currently, there's no way to configure specific devices individually (but that is planned).

All settings at a glance:

```kdl
input {
    keyboard {
        xkb {
            // layout "us"
            // variant "colemak_dh_ortho"
            // options "compose:ralt,ctrl:nocaps"
            // model ""
            // rules ""
            // file "~/.config/keymap.xkb"
        }

        // repeat-delay 600
        // repeat-rate 25
        // track-layout "global"
        numlock
    }

    touchpad {
        // off
        tap
        // dwt
        // dwtp
        // drag false
        // drag-lock
        natural-scroll
        // accel-speed 0.2
        // accel-profile "flat"
        // pinch-sensitivity 1.0
        // scroll-factor 1.0
        // scroll-factor vertical=1.0 horizontal=-2.0
        // scroll-method "two-finger"
        // scroll-button 273
        // scroll-button-lock
        // tap-button-map "left-middle-right"
        // click-method "clickfinger"
        // left-handed
        // disabled-on-external-mouse
        // middle-emulation
    }

    mouse {
        // off
        // natural-scroll
        // accel-speed 0.2
        // accel-profile "flat"
        // scroll-factor 1.0
        // scroll-factor vertical=1.0 horizontal=-2.0
        // scroll-method "no-scroll"
        // scroll-button 273
        // scroll-button-lock
        // left-handed
        // middle-emulation
    }

    trackpoint {
        // off
        // natural-scroll
        // accel-speed 0.2
        // accel-profile "flat"
        // scroll-method "on-button-down"
        // scroll-button 273
        // scroll-button-lock
        // left-handed
        // middle-emulation
    }

    trackball {
        // off
        // natural-scroll
        // accel-speed 0.2
        // accel-profile "flat"
        // scroll-method "on-button-down"
        // scroll-button 273
        // scroll-button-lock
        // left-handed
        // middle-emulation
    }

    tablet {
        // off
        map-to-output "eDP-1"
        // map-to-focused-output
        // map-to-focused-window
        // left-handed
        // calibration-matrix 1.0 0.0 0.0 0.0 1.0 0.0
    }

    touchscreen {
        // off
        map-to-output "eDP-1"
        // calibration-matrix 1.0 0.0 0.0 0.0 1.0 0.0
        // gestures {
        //     min-gesture-fingers 3
        //     exclusion-rects 1280 720 0 0
        //     swipe-trigger-distance 100
        //     edge-start-distance 12
        //     pinch-trigger-distance 100
        //     rotation-trigger-angle 20
        //     tap-wobble-threshold 15
        //     tap-timeout-ms 500
        // }
    }

    // disable-power-key-handling
    // warp-mouse-to-focus
    // focus-follows-mouse max-scroll-amount="0%"
    // workspace-auto-back-and-forth

    // mod-key "Super"
    // mod-key-nested "Alt"
}
```

### Keyboard

#### Layout

In the `xkb` section, you can set layout, variant, options, model and rules.
These are passed directly to libxkbcommon, which is also used by most other Wayland compositors.
See the `xkeyboard-config(7)` manual for more information.

```kdl
input {
    keyboard {
        xkb {
            layout "us"
            variant "colemak_dh_ortho"
            options "compose:ralt,ctrl:nocaps"
        }
    }
}
```

> [!TIP]
>
> <sup>Since: 25.02</sup>
>
> Alternatively, you can directly set a path to a .xkb file containing an xkb keymap.
> This overrides all other xkb settings.
>
> ```kdl
> input {
>     keyboard {
>         xkb {
>             file "~/.config/keymap.xkb"
>         }
>     }
> }
> ```

> [!NOTE]
>
> <sup>Since: 25.08</sup>
>
> If the `xkb` section is empty (like it is by default), niri will fetch xkb settings from systemd-localed at `org.freedesktop.locale1` over D-Bus.
> This way, for example, system installers can dynamically set the niri keyboard layout.
> You can see this layout in `localectl` and change it with `localectl set-x11-keymap`, for example:
>
> ```sh
> $ localectl set-x11-keymap "us" "" "colemak_dh_ortho" "compose:ralt,ctrl:nocaps"
> $ localectl
> System Locale: LANG=en_US.UTF-8
>                LC_NUMERIC=ru_RU.UTF-8
>                LC_TIME=ru_RU.UTF-8
>                LC_MONETARY=ru_RU.UTF-8
>                LC_PAPER=ru_RU.UTF-8
>                LC_MEASUREMENT=ru_RU.UTF-8
>     VC Keymap: us-colemak_dh_ortho
>    X11 Layout: us
>   X11 Variant: colemak_dh_ortho
>   X11 Options: compose:ralt,ctrl:nocaps
> ```
>
> By default, `localectl` will set the TTY keymap to the closest match of the XKB keymap.
> You can prevent that with a `--no-convert` flag, for example: `localectl set-x11-keymap --no-convert "us,ru"`.
>
> These settings are picked up by some other programs too, like GDM.

When using multiple layouts, niri can remember the current layout globally (the default) or per-window.
You can control this with the `track-layout` option.

- `global`: layout change is global for all windows.
- `window`: layout is tracked for each window individually.

```kdl
input {
    keyboard {
        track-layout "global"
    }
}
```

#### Repeat

Delay is in milliseconds before the keyboard repeat starts.
Rate is in characters per second.

```kdl
input {
    keyboard {
        repeat-delay 600
        repeat-rate 25
    }
}
```

#### Num Lock

<sup>Since: 25.05</sup>

Set the `numlock` flag to turn on Num Lock automatically at startup.

You might want to disable (comment out) `numlock` if you're using a laptop with a keyboard that overlays Num Lock keys on top of regular keys.

```kdl
input {
    keyboard {
        numlock
    }
}
```

### Pointing Devices

Most settings for the pointing devices are passed directly to libinput.
Other Wayland compositors also use libinput, so it's likely you will find the same settings there.
For flags like `tap`, omit them or comment them out to disable the setting.

A few settings are common between input devices:

- `off`: if set, no events will be sent from this device.

A few settings are common between `touchpad`, `mouse`, `trackpoint`, and `trackball`:

- `natural-scroll`: if set, inverts the scrolling direction.
- `accel-speed`: pointer acceleration speed, valid values are from `-1.0` to `1.0` where the default is `0.0`.
- `accel-profile`: can be `adaptive` (the default) or `flat` (disables pointer acceleration).
- `scroll-method`: when to generate scroll events instead of pointer motion events, can be `no-scroll`, `two-finger`, `edge`, or `on-button-down`.
  The default and supported methods vary depending on the device type.
- `scroll-button`: <sup>Since: 0.1.10</sup> the button code used for the `on-button-down` scroll method. You can find it in `libinput debug-events`.
- `scroll-button-lock`: <sup>Since: 25.08</sup> when enabled, the button does not need to be held down. Pressing once engages scrolling, pressing a second time disengages it, and double click acts as single click of the the underlying button.
- `left-handed`: if set, changes the device to left-handed mode.
- `middle-emulation`: emulate a middle mouse click by pressing left and right mouse buttons at once.

Settings specific to `touchpad`s:

- `tap`: tap-to-click.
- `dwt`: disable-when-typing.
- `dwtp`: disable-when-trackpointing.
- `drag`: <sup>Since: 25.05</sup> can be `true` or `false`, controls if tap-and-drag is enabled.
- `drag-lock`: <sup>Since: 25.02</sup> if set, lifting the finger off for a short time while dragging will not drop the dragged item. See the [libinput documentation](https://wayland.freedesktop.org/libinput/doc/latest/tapping.html#tap-and-drag).
- `pinch-sensitivity`: <sup>Since: next release</sup> scales the sensitivity of pinch gestures sent to applications.
  Values above `1.0` increase sensitivity, while values below `1.0` decrease it.
- `tap-button-map`: can be `left-right-middle` or `left-middle-right`, controls which button corresponds to a two-finger tap and a three-finger tap.
- `click-method`: can be `button-areas` or `clickfinger`, changes the [click method](https://wayland.freedesktop.org/libinput/doc/latest/clickpad-softbuttons.html).
- `disabled-on-external-mouse`: do not send events while external pointer device is plugged in.

Settings specific to `touchpad` and `mouse`:

- `scroll-factor`: <sup>Since: 0.1.10</sup> scales the scrolling speed by this value.

    <sup>Since: 25.08</sup> You can also override horizontal and vertical scroll factor separately like so: `scroll-factor horizontal=2.0 vertical=-1.0`

Settings specific to `tablet` and `touchscreen`:

- `calibration-matrix`: set to six floating point numbers to change the calibration matrix. See the [`LIBINPUT_CALIBRATION_MATRIX` documentation](https://wayland.freedesktop.org/libinput/doc/latest/device-configuration-via-udev.html) for examples.
    - <sup>Since: 25.02</sup> for `tablet`
    - <sup>Since: 25.11</sup> for `touchscreen` (this node was called `touch` until it was renamed; `touch { }` is a parse error)

Tablets and touchscreens are absolute pointing devices that can be mapped to a specific output like so:

```kdl
input {
    tablet {
        map-to-output "eDP-1"
    }

    touchscreen {
        map-to-output "eDP-1"
    }
}
```

Valid output names are the same as the ones used for output configuration.

<sup>Since: 0.1.7</sup> When a tablet is not mapped to any output, it will map to the union of all connected outputs, without aspect ratio correction.

Settings specific to `tablet`:

- `map-to-focused-output`: <sup>Since: 26.04</sup> will map the tablet to the focused output, takes precedence over `map-to-output`.

- `map-to-focused-window`: <sup>Since: next release</sup> will map the tablet to the focused window's geometry, takes precedence over `map-to-focused-output` and `map-to-output`.
Falls back to those when no window is focused (for example, in the overview).

    When the tablet is also mapped to a specific output via `map-to-output`, the `map-to-focused-window` flag will map the tablet to the active window on that output.
    If the tablet isn't mapped to any specific output, it will map the tablet to the current focused window regardless of where it is.

#### Touchscreen Gestures

<sup>Since: next release</sup>

Settings specific to `touchscreen`:

- `gestures { ... }`: tunes when the compositor claims a touchscreen sequence, and where it declines to. See [Touchscreen Gestures](#touchscreen-gestures) below.

##### `exclusion-rects`

Rectangles inside which the compositor's touchscreen gestures are suppressed, and touches are always forwarded to the client.
This is the L3 shape of Android's `systemGestureExclusionRects`: a region of the screen where a compositor gesture must not fire, so that a video player, a drawing app or a map can own the area without the compositor guessing from an app-id match.

Each rectangle is four arguments in the order `w h x y`, meaning "a box this big, at this position".
The coordinates are **global logical pixels**, the same space as a touch-down's position: the origin is the top-left corner of the bounding box of all outputs, and each output contributes its logical size at its `position` from the `outputs {}` block.
A rect is therefore *not* per-output, and one authored for a single-monitor layout will be in the wrong place once a second monitor is added.

Negative coordinates are legal and mean a monitor placed to the left of, or above, that origin.
Each value must be between `-1000000` and `1000000`.
Rects are half-open on the right and bottom edges, so two abutting rects partition the plane without both claiming the boundary.

This is **one** node carrying a run of `w h x y` quadruples, not a repeated node.
A run whose length is not a multiple of four is a hard parse error, and writing two `exclusion-rects` lines is rejected as well, since niri's config grammar disallows duplicate child nodes.

```kdl
input {
    touchscreen {
        gestures {
            // Full-screen video player, plus a 400x300 region on a second
            // monitor sitting to the right.
            exclusion-rects 1280 720 0 0 400 300 1700 0
        }
    }
}
```

##### Tuning knobs

The other settings live in the same `gestures { ... }` block:

- `min-gesture-fingers`: how many simultaneous contacts arm the compositor when the sequence did not start in an edge zone or under Mod. Valid values are from `2` to `10`, the default is `3`. Contacts below this count are forwarded to the client, so two-finger scroll/zoom passthrough keeps working. The value is rounded to the nearest whole number.
- `swipe-trigger-distance`: pixels of centroid movement required before a swipe gesture latches. Lower values feel more responsive but risk triggering on incidental finger drift. The default is `100`.
- `edge-start-distance`: width (in pixels) of the screen-edge start zone. A touch must *begin* within this distance from an edge for it to count as a `TouchEdge` gesture; touches starting farther in are treated as regular swipes. The default is `12`.
- `pinch-trigger-distance`: pixels of `|spread_change|` required before a pinch gesture latches. The default is `100`.
- `rotation-trigger-angle`: cumulative rotation (in **degrees**) required before a rotation gesture latches. Valid values are from `0` to `360`, the default is `20`.
- `tap-wobble-threshold`: maximum per-finger displacement (in pixels) allowed during a tap gesture. If any single finger moves farther than this from its initial landing position, the tap candidate is killed and the gesture can only resolve as swipe/pinch/rotate. The default is `15`.
- `tap-timeout-ms`: maximum duration (in milliseconds) from the first finger landing to all fingers lifting for a tap to fire. Taps slower than this are discarded. The default is `500`.

##### Gesture Triggers

The following bind triggers recognise touchscreen gestures.
Each one takes its parameters as KDL properties on the bind, and all of them also accept the standard bind properties such as `repeat`, `cooldown-ms`, `allow-when-locked` and `emit-progress`.
Modifiers work as usual, for example `Mod+TouchEdge edge="left"`.
None of these families accepts an `edge=` or `zone=` property except `TouchEdge`, and none accepts `fingers=` except the five that need a finger count.

`fingers=` is required for `TouchSwipe`, `TouchPinch`, `TouchRotate`, `TouchTap` and `TouchTapHoldDrag`, and must be from `3` to `10`.
The floor of 3 exists so that two-finger client passthrough (scrolling and zooming) keeps working.

```kdl
binds {
    // Three fingers swiped up: close the focused window.
    Mod+TouchSwipe fingers=3 direction="up" { close-window; }

    // Three fingers pinched outwards: widen the focused window.
    Mod+TouchPinch fingers=3 direction="out" { switch-preset-window-width; }

    // Three fingers rotated clockwise: take a screenshot.
    Mod+TouchRotate fingers=3 direction="cw" { screenshot-screen; }

    // Three fingers tapped: toggle the overview.
    TouchTap fingers=3 { toggle-overview; }

    // Three fingers, held then dragged in any direction: move a window.
    // Add direction="up|down|left|right" to restrict the initial
    // movement direction; without it the trigger is omnidirectional.
    Mod+TouchTapHoldDrag fingers=3 { move-window-down; }

    // A single-finger swipe starting within edge-start-distance of the
    // bottom edge, in its centre third: switch to the workspace below.
    Mod+TouchEdge edge="bottom" zone="center" { focus-workspace-down; }
}
```

`TouchSwipe` requires `direction=`, which can be `up`, `down`, `left` or `right`.
`TouchPinch` requires `direction=`, which can be `in` or `out`.
`TouchRotate` requires `direction=`, which can be `cw` or `ccw`.
`TouchTap` does not accept a `direction=`.

`TouchTapHoldDrag` accepts an optional `direction=`; omitting it matches any direction.

`TouchEdge` requires `edge=`, which can be `left`, `right`, `top` or `bottom`, and accepts an optional `zone=`.
It does not accept `fingers=` or `direction=`.
`zone=` picks one of the three thirds along the edge's perpendicular axis, and the vocabulary rotates with the edge: the `top` and `bottom` edges take `zone="left"|"center"|"right"`, while the `left` and `right` edges take `zone="top"|"center"|"bottom"`.
When several binds match one gesture, a zoned bind is preferred over an unzoned one; an unzoned `TouchEdge` acts as the fallback for any zone.

##### `emit-progress`

Without it, a recognised gesture produces no IPC events at all.
With `emit-progress true`, niri mirrors the recognised gesture onto the IPC event stream as `TouchGestureBegin`, `TouchGestureUpdate` and `TouchGestureEnd`, so an external overlay can draw live gesture progress.
It gates only that event stream; it never causes the bind to fire twice.

```kdl
binds {
    TouchSwipe fingers=3 direction="up" emit-progress=true { close-window; }
}
```

Note that `emit-progress` is a property and takes a value: a bare `emit-progress` on its own is a syntax error, exactly like a bare `repeat` or `allow-when-locked`.


### General Settings

These settings are not specific to a particular input device.

#### `disable-power-key-handling`

By default, niri will take over the power button to make it sleep instead of power off.
Set this if you would like to configure the power button elsewhere (i.e. `logind.conf`).

```kdl
input {
    disable-power-key-handling
}
```

#### `warp-mouse-to-focus`

Makes the mouse warp to newly focused windows.

Does not make the cursor visible if it had been hidden.

```kdl
input {
    warp-mouse-to-focus
}
```

By default, the cursor warps *separately* horizontally and vertically.
I.e. if moving the mouse only horizontally is enough to put it inside the newly focused window, then the mouse will move only horizontally, and not vertically.

<sup>Since: 25.05</sup> You can customize this with the `mode` property.

- `mode="center-xy"`: warps by both X and Y coordinates together.
So if the mouse was anywhere outside the newly focused window, it will warp to the center of the window.
- `mode="center-xy-always"`: warps by both X and Y coordinates together, even if the mouse was already somewhere inside the newly focused window.

```kdl
input {
    warp-mouse-to-focus mode="center-xy"
}
```

#### `focus-follows-mouse`

Focuses windows and outputs automatically when moving the mouse over them.

```kdl
input {
    focus-follows-mouse
}
```

<sup>Since: 0.1.8</sup> You can optionally set `max-scroll-amount`.
Then, focus-follows-mouse won't focus a window if it will result in the view scrolling more than the set amount.
The value is a percentage of the working area width.

```kdl
input {
    // Allow focus-follows-mouse when it results in scrolling at most 10% of the screen.
    focus-follows-mouse max-scroll-amount="10%"
}
```

```kdl
input {
    // Allow focus-follows-mouse only when it will not scroll the view.
    focus-follows-mouse max-scroll-amount="0%"
}
```

#### `workspace-auto-back-and-forth`

Normally, switching to the same workspace by index twice will do nothing (since you're already on that workspace).
If this flag is enabled, switching to the same workspace by index twice will switch back to the previous workspace.

Niri will correctly switch to the workspace you came from, even if workspaces were reordered in the meantime.

```kdl
input {
    workspace-auto-back-and-forth
}
```

#### `mod-key`, `mod-key-nested`

<sup>Since: 25.05</sup>

Customize the `Mod` key for [key bindings](./Configuration:-Key-Bindings.md).
Only valid modifiers are allowed, e.g. `Super`, `Alt`, `Mod3`, `Mod5`, `Ctrl`, `Shift`.

By default, `Mod` is equal to `Super` when running niri on a TTY, and to `Alt` when running niri as a nested winit window.

> [!NOTE]
> There are a lot of default bindings with Mod, none of them "make it through" to the underlying window.
> You probably don't want to set `mod-key` to Ctrl or Shift, since Ctrl is commonly used for app hotkeys, and Shift is used for, well, regular typing.

```kdl
// Switch the mod keys around: use Alt normally, and Super inside a nested window.
input {
    mod-key "Alt"
    mod-key-nested "Super"
}
```

### Overview

<sup>Since: 25.02</sup>

The `gestures` config section contains gesture settings.
For an overview of all niri gestures, see the [Gestures](./Gestures.md) wiki page.

Here's a quick glance at the available settings along with their default values.

```kdl
gestures {
    dnd-edge-view-scroll {
        trigger-width 30
        delay-ms 100
        max-speed 1500
    }

    dnd-edge-workspace-switch {
        trigger-height 50
        delay-ms 100
        max-speed 1500
    }

    hot-corners {
        // off
        top-left
        // top-right
        // bottom-left
        // bottom-right
    }

    // Draw a dot under every finger, or only during multi-finger gestures.
    // show-touch-points all
    // show-touch-points gestures
}
```

### `dnd-edge-view-scroll`

Scroll the tiling view when moving the mouse cursor against a monitor edge during drag-and-drop (DnD).
Also works on a touchscreen.

This will work for regular drag-and-drop (e.g. dragging a file from a file manager), and for window interactive move when targeting the tiling layout.

The options are:

- `trigger-width`: size of the area near the monitor edge that will trigger the scrolling, in logical pixels.
- `delay-ms`: delay in milliseconds before the scrolling starts.
Avoids unwanted scrolling when dragging things across monitors.
- `max-speed`: maximum scrolling speed in logical pixels per second.
The scrolling speed increases linearly as you move your mouse cursor from `trigger-width` to the very edge of the monitor.

```kdl
gestures {
    // Increase the trigger area and maximum speed.
    dnd-edge-view-scroll {
        trigger-width 100
        max-speed 3000
    }
}
```

### `dnd-edge-workspace-switch`

<sup>Since: 25.05</sup>

Scroll the workspaces up/down when moving the mouse cursor against a monitor edge during drag-and-drop (DnD) while in the overview.
Also works on a touchscreen.

The options are:

- `trigger-height`: size of the area near the monitor edge that will trigger the scrolling, in logical pixels.
- `delay-ms`: delay in milliseconds before the scrolling starts.
Avoids unwanted scrolling when dragging things across monitors.
- `max-speed`: maximum scrolling speed; 1500 corresponds to one screen height per second.
The scrolling speed increases linearly as you move your mouse cursor from `trigger-width` to the very edge of the monitor.

```kdl
gestures {
    // Increase the trigger area and maximum speed.
    dnd-edge-workspace-switch {
        trigger-height 100
        max-speed 3000
    }
}
```

### `hot-corners`

<sup>Since: 25.05</sup>

Put your mouse at the very top-left corner of a monitor to toggle the overview.
Also works during drag-and-dropping something.

`off` disables the hot corners.

```kdl
// Disable the hot corners.
gestures {
    hot-corners {
        off
    }
}
```

<sup>Since: 25.11</sup> You can choose specific hot corners by name: `top-left`, `top-right`, `bottom-left`, `bottom-right`.
If no corners are explicitly set, the top-left corner will be active by default.

```kdl
// Enable the top-right and bottom-right hot corners.
gestures {
    hot-corners {
        top-right
        bottom-right
    }
}
```

You can also customize hot corners per-output [in the output config](./Configuration:-Outputs.md#hot-corners).

### `show-touch-points`

<sup>Since: 26.04</sup>

Draw a translucent dot under each finger touching the screen.
The dots fade in when a finger lands and fade out when it lifts, and the compositor draws them on top of everything, so a screen recording captures them.
This is for demo videos and for debugging gestures, and it is off by default.

This setting is purely observational.
It never consumes or alters touch events, so turning it on cannot change what any gesture does, and a client still receives its touches exactly as before.

The options are:

- `all`: every touch point gets a dot, including a single finger tapping or scrolling.
- `gestures`: dots appear only once enough fingers are down concurrently to drive a gesture (three by default), so ordinary taps and scrolling stay clean.
  A finger that landed before the threshold was reached gets its dot at the same moment the gesture becomes possible, rather than popping in late.

Omitting the setting is equivalent to `off`.

```kdl
// Show a dot under every finger.
gestures {
    show-touch-points all
}
```

```kdl
// Only show dots while a multi-finger gesture is possible.
gestures {
    show-touch-points gestures
}
```

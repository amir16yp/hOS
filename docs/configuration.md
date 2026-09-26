# HOSWM session configuration

[Back to the project guide](../README.md)

This page describes the desktop session's own settings. System settings —
network, power, time and sound — live in `/etc/hos` and belong to the
services that read them; see [init and system services](services.md). The
`hos-settings` application edits both: it changes a running service through
the service API and writes the same change to `/etc/hos`, and it edits the
file described here in place, keeping its comments.

HOSWM reads `~/.hoswm/config.ini` when a session starts. The file is optional:
if it is missing, the session writes the documented defaults there once, so the
file on disk always shows the current settings. Changes apply at the next
session start.

Set `HOSWM_HOME` to use a different directory; `hos-notifications` and the
other applications honour the same variable.

Lines beginning with `#` or `;` are comments. A line HOSWM cannot understand
becomes a notification naming the line number, and is otherwise skipped: a typo
in one setting never keeps the session from starting and never discards the
rest of the file. Relative paths resolve against the configuration directory,
and colors are `0xAARRGGBB` or `#RRGGBB`.

## Sections

| Section | Setting | Meaning |
| --- | --- | --- |
| `session` | `accent` | Color of the application name in the menu bar and of session notifications |
| `session` | `wallpaper` | QOI image drawn behind the windows, `wallpaper.qoi` by default; scaled to cover the screen, keeping its proportions. Without the file the desktop is its plain background color |
| `menubar` | `enabled` | Draw the menu bar across the top of the screen; windows never cover it |
| `menubar` | `clock` | Show a UTC clock at the right of the bar |
| `toasts` | `corner` | `top-left`, `top-right`, `bottom-left` or `bottom-right` |
| `toasts` | `duration_ms` | Display time when a caller asks for the default; clamped to 500–60000 |
| `toasts` | `max_visible` | Notifications on screen at once, 1–8; the oldest is retired early |
| `toasts` | `database` | Notification log path, `toastdb` by default |
| `screenshots` | `directory` | Where captures are written, as QOI images named by capture time |
| `dock` | `item` | One dock button; repeat the key, left to right |
| `shortcuts` | *action* | `[ctrl+][alt+][shift+]key`, or `none` to unbind |

The wallpaper is read once, when the session starts: write the file, then
start the session again to see it. A file that is present but unreadable is
reported as a notification and the plain background is used.

A `[dock]` section replaces the default dock entirely, so listing no items
gives an empty dock.

## Dock items

```ini
[dock]
; label | command | symbol | color | icon
item = Terminal | /bin/hos-terminal | >_ | 0xff72dbac | icons/terminal.qoi
item = Exit | @exit | X | 0xffef6976
```

Only the label and command are required. The command is an executable to run,
or `@exit` to leave the session (logging out when the session started from the
graphical login). The symbol is up to two characters, drawn when no icon is
given or when the icon cannot be read. The icon is a [QOI](https://qoiformat.org)
image of any size, scaled to the dock button; screenshots taken by HOSWM use the
same format, so a cropped screenshot works as an icon.

Windows appear as further dock buttons after the configured items, as before.

## Shortcuts

| Action | Default | Effect |
| --- | --- | --- |
| `exit` | `ctrl+alt+escape` | Leave the session unless an application protects critical work |
| `switch_window` | `alt+tab` | Cycle visible windows; Shift cycles backwards |
| `dismiss` | `escape` | Close a context menu, then an open bar menu, then clear notifications |
| `copy`, `cut`, `paste`, `select_all` | `ctrl+c/x/v/a` | Text editing in GUI controls |
| `delete` | unbound | Delete the selection in a text control |
| `screenshot` | `print` | Capture the screen |
| `screenshot_window` | `alt+print` | Capture the focused window |

Key names are single characters, `f1`–`f12`, `escape`, `tab`, `enter`, `space`,
`backspace`, `delete`, `insert`, `home`, `end`, `pageup`, `pagedown`, `up`,
`down`, `left`, `right`, `print`, and `code:N` for any other evdev code. The
first binding that matches a key press wins.

Editing shortcuts also match with Shift held, so `Ctrl+Shift+C` copies in a GUI
window. While a window that consumes raw keyboard input is focused — the
terminal, for instance — editing shortcuts require Shift, leaving `Ctrl+C` to
the shell. Cut is never taken from such a window.

## Notifications

Any application can post a notification with `hos_toast` in C,
`hoswm::client::Client::toast` in Rust, or the `hos-toast` command:

```sh
hos-toast "Backup finished"
hos-toast --color red --ms 8000 "Disk almost full"
hos-toast --list
```

The session also posts its own: input devices arriving and leaving,
configuration problems, screenshots, and applications that fail to start.
Clicking a notification dismisses it early.

When a notification leaves the screen it is appended to `~/.hoswm/toastdb`,
with the time it appeared, the duration requested, the time it was actually
shown, its color and its text. `hos-notifications` — the dock's `!` button —
lists the log, newest first, and reloads when the file changes.

### Log format

All integers are little-endian. A 16-byte header is followed by records:

```text
header: "HOSTOAST" magic (8) | u16 version | u16 header length | u32 reserved
record: u32 length (whole record, including this field)
        u64 unix milliseconds when the notification appeared
        u32 requested milliseconds
        u32 milliseconds actually shown
        u32 color (0xAARRGGBB)
        u32 text length in bytes
        UTF-8 text, zero-padded to a 4-byte boundary
        u32 CRC-32 of the record between the length and this field
```

Records are self-delimiting and checksummed, and the file is only ever
appended to. A log truncated by a power loss is read up to the last intact
record; `hos-notifications` reports the damaged tail instead of hiding it.
`hoswm::toast` implements this format.

## Image previews

`hos-files` shows a preview of the selected QOI image. Previews live in
`~/.hoswm/previews`, and both `hos-files` and `hos-image --preview` fill the
cache, so a directory browsed once is fast to browse again. Each entry is a QOI image named after
the source path, its modification time and its size, so an edited image is
never shown from a stale entry; the cache keeps its 256 newest entries.

```sh
hos-image picture.qoi              # view, with zoom, panning and scaling modes
hos-image --preview picture.qoi    # write a preview and print its path
```

Only QOI images are supported. `hoswm::preview` implements the cache, and
accepts a different cache directory for callers that want one.

## Screenshots

`Print` captures the screen and `Alt+Print` the focused window, both written to
`~/.hoswm/screenshots` as `shot-YYYYMMDD-HHMMSS-mmm.qoi`, with a notification
naming the file. The same items are in the bar's **System** menu. Captures come
from the frame that was actually presented, so they never contain the
notification announcing them. `hoswm::qoi` encodes and decodes the format, and
can save any surface.

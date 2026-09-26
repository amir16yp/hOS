# HOSWM application development

[Back to the project guide](../README.md)

HOSWM exposes a language-neutral local window ABI. The public C interface is [hoswm.h](../HOSWM/include/hoswm.h), and Rust applications can use the `hoswm::client` module. The terminal, installer and About window are independent Rust executables using the same ABI.

## Build and run the example

On the Linux build host, from the project root:

```sh
tooling/build.sh hoswm
cc -std=c11 -O2 -Wall -Wextra -Werror -static \
  -I HOSWM/include HOSWM/examples/hello_gui.c \
  .build/out/libhoswm.a -o .build/out/hos-hello
```

The second command demonstrates linking; the build stage already builds that example. If using `HOS_BUILD_DIR`, adjust the output/library paths. Static linking avoids requiring additional shared libraries in the guest.

Rust applications can depend on the HOSWM crate and build as ordinary binaries:

```rust
use hoswm::client::{Client, WINDOW_DEFER_CLOSE, WINDOW_RAW_INPUT};
let client = Client::connect()?;
let window = client.create("Example", 400, 240, 0xff72dbac)?;
client.flags(window, WINDOW_RAW_INPUT | WINDOW_DEFER_CLOSE)?;
```

The `hoswm` build stage compiles and packages `hos-terminal`, `hos-installer` and `hos-about` as `/bin` executables. Dock clicks launch those programs. The window manager assigns each new window to its creating process and closes its windows when that process exits.

Run `hos-hello` inside a terminal in the running guest desktop. It is included in both live and installed systems, together with `/usr/include/hoswm.h`, `/usr/lib/libhoswm.a` and `/usr/share/hoswm/hello_gui.c`. A compiler is not bundled in the guest; compile applications on the host.

Alternatively, define `HOSWM_IMPLEMENTATION` before including `hoswm.h` in exactly one C translation unit and compile without `libhoswm.a`. Do not combine both implementations. The header provides C++ linkage guards for API declarations.

## API conventions

Window handles are `uint32_t` values (`HosWindow`). Creation returns zero on failure; other calls return `-1` on error and set `errno`. `hos_window_poll` returns `1` for an event and `0` for an empty queue. Server validation errors become `EPROTO`.

Coordinates describe window content pixels, excluding the title bar and border. Colors use `0xAARRGGBB`. Strings are UTF-8 and NUL-terminated at the C boundary; limits are measured in bytes. Client-supplied pixel buffers are made opaque by the server, even though the internal Rust drawing API supports alpha blending.

| Function | Purpose |
| --- | --- |
| `hos_session_info` | Query ABI version and logical desktop width/height; output pointers may be NULL |
| `hos_gui_remove_control` | Remove a control, clear its focus/press state and discard its queued click/change/submit events |
| `hos_window_create`, `hos_gui_window_create` | Create a window; the GUI name is an alias |
| `hos_window_create_with_size` | Create a window with separate internal render and final display sizes |
| `hos_message_box` | Create a message window with dismissal controls |
| `hos_window_close` | Close a window |
| `hos_window_set_flags` | Enable raw input, deferred close requests, protection from session exit during critical work, or user resizing |
| `hos_window_set_fps` | Set the window's requested update/render rate from 1 to 240 FPS |
| `hos_clipboard_set`, `hos_clipboard_get` | Share UTF-8 text through the session clipboard |
| `hos_window_size` | Query current content width, height and minimized state |
| `hos_window_size_info` | Query internal render size, final display size, minimized state and FPS |
| `hos_window_present` | Submit a complete pixel buffer matching the current content size |
| `hos_window_run` | Run the client's update and render callbacks at the requested FPS |
| `hos_window_poll` | Remove one queued event, or report no event |
| `hos_gui_control` | Create or replace a control by ID |
| `hos_gui_label`, `hos_gui_button`, `hos_gui_textbox` | Convenience control constructors |
| `hos_gui_set_text`, `hos_gui_get_text` | Set or retrieve control text |
| `hos_window_set_menus` | Replace this window's menu bar menus |
| `hos_message_box_open`, `hos_message_box_ask` | Ask a question with answer buttons |
| `hos_toast` | Show a notification, logged to the session notification log |

Create windows with content widths of **180–796** and heights of **60–498** pixels. Titles allow up to 128 bytes; message bodies allow 2,048 bytes; control text allows 1,024 bytes. Control IDs must be nonzero and are local to a window. Control dimensions must be at least 16×12 and fit within the server's 796×498 control coordinate bounds; controls outside the current content are clipped.

A window can be maximized and restored by the user. A window created with `hos_window_create_with_size` keeps its internal render size while the window manager scales it to the final frame, so maximizing never forces a client buffer to grow with the display. `hos_window_size_info` reports both sizes. A window that sets `HOS_WINDOW_RESIZABLE` can also be resized by dragging any edge or corner of its frame, which the window manager marks with a grip in the bottom-right corner; the grab band reaches a few pixels outside the frame. Resizing keeps the window on the desktop and stops at the 180x60 minimum content size, and a maximized window is resized with its restore button instead. Each new size arrives as `HOS_EVENT_RESIZE` and is reported by `hos_window_size`. Pixel clients should query dimensions after a resize and present a buffer with that exact internal size. `hos_window_set_fps` and `hos_window_run` let each client choose its own update/render cadence. GUI controls use fixed coordinates and do not automatically reflow, which is why a window whose layout is fixed, such as a message box, leaves the flag off. The bundled terminal sets it and reflows its grid onto the new size.

## Events

`HosEvent` contains `kind`, `control`, `text_length`, and `text[1025]`. The client adds a NUL terminator, but use `text_length` when processing key bytes. Poll regularly and sleep between empty polls, as the example does.

| Event | Meaning |
| --- | --- |
| `HOS_EVENT_NONE` | Queue is empty |
| `HOS_EVENT_CLICK` | Button activated; `control` identifies it |
| `HOS_EVENT_CHANGE` | Textbox edited; `text` contains its value |
| `HOS_EVENT_SUBMIT` | Enter pressed in a textbox |
| `HOS_EVENT_RESIZE` | `control` contains content width; `text` contains decimal height |
| `HOS_EVENT_POINTER` | Left press on window content outside a control; `text` is `x y` |
| `HOS_EVENT_KEY` | Key bytes delivered to an application window |
| `HOS_EVENT_CLOSED` | Polled window no longer exists |
| `HOS_EVENT_RAW_POINTER` | Raw input mode: control 1 is left button, 2 right button, 0 motion; text is `x y action` (`1` press, `0` release/motion, `2` right press) |
| `HOS_EVENT_CLOSE_REQUEST` | The user requested that the window close; close it with `hos_window_close`, or keep it open |
| `HOS_EVENT_MENU` | A menu bar item was chosen; `control` is its ID and `text` its label |

For `HOS_EVENT_KEY`, `control` carries modifier bits: 1 for Ctrl, 2 for Shift and 4 for Alt. `HOS_EVENT_RAW_POINTER` sends content coordinates and the button action described above.

`hos_window_set_flags` accepts `HOS_WINDOW_RAW_INPUT`, `HOS_WINDOW_DEFER_CLOSE`, `HOS_WINDOW_PROTECT_EXIT` and `HOS_WINDOW_RESIZABLE`. Raw mode routes keyboard bytes and pointer events to the client instead of GUI controls. Deferred close turns the titlebar close button into a close-request event. Protect-exit prevents Escape or the dock exit action from stopping the session while the application reports critical work, such as disk installation. Clear that flag when the work ends. The flags word replaces every flag at once, so name each one the window still wants.

Existing text constructors default to **non-selectable**. Use `hos_gui_label_ex`, `hos_gui_textbox_ex`, or `hos_gui_control_ex` with a final `bool selectable` argument to opt in:

```c
hos_gui_label_ex(window, 1, 16, 16, 300, 24, "Drag to copy this text", true);
hos_gui_textbox_ex(window, 2, 16, 48, 300, 28, "Editable text", true);
```

Left-drag selects text. Right-click opens Copy, Cut, Paste, Delete, and Select all; unavailable actions are disabled. Labels are read-only. Editable selections support Ctrl+C/X/V/A, Backspace/Delete, Left/Right, and Home/End. Typing or pasting replaces the selection and emits `HOS_EVENT_CHANGE`. Clipboard contents are shared within the HOSWM session, not with the host OS. Paste into textboxes strips control characters and rejects edits exceeding the 1,024-byte limit.

The separate terminal application supports selecting visible output by dragging, copies it with Ctrl+Shift+C or right-click, pastes with Ctrl+Shift+V and selects the visible screen with Ctrl+Shift+A. Ctrl+C and other unshifted control keys retain their shell meaning. It has no scrollback selection. The terminal supports basic/bright ANSI colors, 256-color (`38/48;5;n`) and true-color (`38/48;2;r;g;b`) SGR, reset, bold and reverse video.

Shortcuts are configurable in `~/.hoswm/config.ini`; the defaults are described here and in the [configuration guide](configuration.md). Alt+Tab cycles visible windows in focus order; hold Alt and press Tab again to continue, or Shift+Tab to reverse. Releasing Alt commits the new focus. Minimized windows remain available from the dock. Escape dismisses an open context menu. Tab cycles through buttons/textboxes. Enter or Space activates a focused button. Enter and Escape dismiss message boxes.

Selection uses retained text/control layout, not pixels submitted through `hos_window_present` or the low-level font rasterizer. UTF-8 selection uses character boundaries; keyboard entry currently uses the US ASCII keymap. Context menus live in `HOSWM/src/context_menu.rs`, shortcut definitions/window cycling in `HOSWM/src/shortcuts.rs`, and text layout/selection in `HOSWM/src/text.rs`.

## Menu bar

HOSWM draws one menu bar across the top of the screen, like a classic desktop:
the name of the focused window, a **System** menu, and that window's own menus.
Windows never cover the bar. Menus are retained by the window manager, so an
application declares them once and is only involved when an item is chosen.

```c
static const HosMenuItem edit_items[] = {
    {1, 0, "Copy", "Ctrl+Shift+C"},
    {0, HOS_MENU_SEPARATOR, NULL, NULL},
    {2, HOS_MENU_CHECKED, "Wrap lines", NULL},
    {3, HOS_MENU_DISABLED, "Undo", NULL},
};
static const HosMenu menus[] = {{"Edit", edit_items, 4}};
hos_window_set_menus(window, menus, 1);
```

```rust
use hoswm::client::{Menu, MenuItem};
client.set_menus(window, &[Menu::new("Edit", vec![
    MenuItem::new(1, "Copy").shortcut("Ctrl+Shift+C"),
    MenuItem::rule(),
    MenuItem::new(2, "Wrap lines").checked(true),
])])?;
```

Choosing an item queues `HOS_EVENT_MENU` with the item ID in `control` and its
label in `text`. Separators and disabled items are never reported. Item IDs
must be nonzero and are local to the window; the shortcut string is a hint
drawn at the right of the item, and does not create a binding.

A window may declare at most 8 menus of 32 items each, with 32-byte titles,
48-byte labels and 16-byte shortcut hints. Passing a count of zero removes the
window's menus. Menus belong to the focused, non-minimized window; the System
menu is always present and offers screenshots, clearing notifications and
leaving the session. The terminal and `hos-notifications` both use this API.

## Message boxes

`hos_message_box` still shows a note with a single dismiss button. To ask a
question, `hos_message_box_ask` opens a dialog and waits:

```c
if (hos_message_box_ask("Delete", "Delete report.qoi?",
                        HOS_BUTTONS_YES_NO, HOS_QUESTION) == HOS_ANSWER_YES) {
    remove("report.qoi");
}
```

The button set is `HOS_BUTTONS_OK`, `HOS_BUTTONS_OK_CANCEL`,
`HOS_BUTTONS_YES_NO` or `HOS_BUTTONS_YES_NO_CANCEL`, and the severity
(`HOS_INFO`, `HOS_WARNING`, `HOS_ERROR`, `HOS_QUESTION`) colors the window.
The answer is `HOS_ANSWER_OK`, `_CANCEL`, `_YES`, `_NO`, or `HOS_ANSWER_CLOSED`
when the user closed the window instead of answering. Enter answers with the
rightmost button and Escape with Cancel, or with OK when that is the only
button.

`hos_message_box_ask` blocks the calling program, not the session: other
windows keep running. Use `hos_message_box_open` instead to keep working while
the dialog is open; the answer arrives as `HOS_EVENT_CLICK` with the answer in
`control`, and the program closes the window itself. The dialog stays on screen
until then, so an answer is never lost with the window. The Rust client offers
the same pair as `Client::ask` and `Client::message`.

## Notifications

`hos_toast(text, color, milliseconds)` shows a notification in the corner
chosen by `~/.hoswm/config.ini`, with zero milliseconds meaning the configured
default and the value otherwise clamped to 500–60000. Text is limited to 512
bytes. Notifications do not belong to a window, and outlive the program that
posted one. Each is appended to the session notification log when it leaves the
screen; see the [configuration guide](configuration.md) for the log format, the
`hos-toast` command and the `hos-notifications` browser.

## Sound

Audio does not travel through the window server. An application plays sound by
talking to `hos-soundd`, which mixes every open stream and writes the result to
the card, so several windows can play at the same time. See
[init and system services](services.md#hos-soundd) for the service side.

A short sound is played by the service, which reads and decodes the file
itself. hOS ships no sound files of its own yet, so this plays whatever the
system has:

```c
hos_sound_play_file("/root/notify.wav");   /* uncompressed 8- or 16-bit WAV */
```

Generated audio is written frame by frame. Samples are interleaved signed
16-bit; the service resamples to whatever the card is running at, and a write
blocks while the mixer catches up, so a program can produce audio in a loop at
its own pace:

```c
HosSound *sound = hos_sound_open(48000, 2, "hos-hello");     /* NULL on failure */
int16_t frames[960 * 2];
for (size_t i = 0; i < 960; i++) {
    int16_t value = (int16_t)(sinf(i * 0.1f) * 8000);
    frames[i * 2] = frames[i * 2 + 1] = value;
}
hos_sound_write(sound, frames, 960 * 2);
hos_sound_close(sound);
```

The same calls exist in Rust as `hoswm::audio::play_file` and
`hoswm::audio::Playback::open`, which returns a value with `write(&[i16])`.

The volume, mute state and default device belong to the system rather than to
one window: `hos_sound_volume`, `hos_sound_set_volume` and
`hos_sound_set_muted` in C, `hoswm::audio::volume`, `set_volume`,
`change_volume` and `set_muted` in Rust. Changes are visible to every
application and to the Sound page of `hos-settings`. A desktop user may change
the volume; the default device is a system setting and needs root.

Streams carry the name given to `hos_sound_open`, which is what
`hosctl sound streams` and the settings application show. Closing the handle
ends the stream once the service has played what it already holds; a program
that exits without closing is cleaned up when its socket closes.

## Session transport

The server listens on `/tmp/hoswm-<effective-uid>/session.sock`, with directory mode `0700` and socket mode `0600`. It refuses to replace a live session. C clients use the same default path; `HOSWM_SOCKET` overrides the client destination, primarily for testing. It does not change the normal server's bind path.

Each C call opens a Unix stream socket, sends one request, reads one response and closes the connection. Send/receive timeouts are three seconds. The server handles up to 32 active connections with bounded per-loop processing and a three-second connection lifetime.

The wire format uses little-endian 32-bit integers. A request is a length prefix followed by an opcode and its payload. A response is a length prefix followed by a status word (`0` for success) and a payload. Length prefixes exclude their own four bytes; the response length includes the status word. Wire strings use a byte length followed by UTF-8 bytes, without the C NUL terminator.

| Opcode | Operation |
| --- | --- |
| 0 | Query ABI version and logical desktop size |
| 1 | Create window |
| 2 | Create message box |
| 3 | Present pixels |
| 4 | Close window |
| 5 | Poll event |
| 6 | Add/replace control |
| 7 | Set control text |
| 8 | Query window size/state |
| 9 | Get control text |
| 10 | Remove control (window ID, control ID); empty success payload |
| 11 | Add/replace selectable control: opcode 6 fields, plus a 0/1 selectable word before the text length |
| 12 | Set window flags (window ID, flags word: 1 raw input, 2 deferred close, 4 protect exit, 8 resizable) |
| 13 | Set session clipboard (byte length, UTF-8 text) |
| 14 | Get session clipboard |
| 15 | Show a notification (color, milliseconds, text); no window ID |
| 16 | Set window menus (window ID, menu count, then per menu: title, item count, and per item: ID, flags, label, shortcut) |
| 17 | Create answer dialog |
| 18 | Create window with internal and final sizes |
| 19 | Query internal size, final size, minimized state and FPS |
| 20 | Set window FPS |

Control removal returns `EPROTO` for an unknown window/control. IDs can be reused after removal. Opcodes 10–16 are additive extensions to ABI v1; older servers reject them with `EPROTO`.

The header declares ABI version 1. Operations 12–16 extend ABI v1. Use the C API or consult `abi.rs` for exact payload field order and validation. The protocol does not isolate applications that share a user session; windows are owned by their creating process and are removed when that process exits.

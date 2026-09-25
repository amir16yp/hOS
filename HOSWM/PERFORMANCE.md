HOSWM targets 60 Hz (16.667 ms per frame) while continuing to process input between frames. Idle sessions do not redraw. A late frame skips expired deadlines instead of adding rendering time to a fixed sleep or replaying a backlog.

Rendering and input changes:

- Linux `ppoll` wakes on keyboard/mouse events, terminal output, IPC, DRM page-flip completion and an inotify watch on `/dev/input`, so plugging a device in is noticed without polling it. Devices are also swept once a second in case inotify is unavailable. Input queues are drained with bounded work; drawing is batched at frame deadlines. A 100 ms idle timeout services installer progress, notification expiry, the menu bar clock and process lifecycle checks; an idle desktop with nothing on screen still does not redraw.
- DRM/KMS is tried automatically, preferring an 800×600 mode nearest 60 Hz. Supported drivers use two scanout buffers and asynchronous, vblank-synchronized page flips. Each buffer tracks its own damage; the compositor never writes a buffer with a flip outstanding.
- DRM hardware cursors move independently of the desktop image. Drivers without cursor support use a saved-background software cursor. Ordinary pointer motion does not recompose windows; dragging, selection and hover changes invalidate the scene.
- Both output paths compare against their previous contents and upload changed scanline spans. DRM damage rectangles are merged and respect the kernel's 256-clip limit. fbdev keeps `write(2)` damage notification, scales to the firmware mode, preserves stride/virtual pixels and combines contiguous uploads.
- Opaque fills and clipped image copies use bulk slice operations. Scratch surfaces and input/poll buffers reuse allocations.

The compositor still draws window contents on the CPU. Hardware acceleration here means KMS scanout, page flips and cursor planes; this does not add an OpenGL/Vulkan compositor. fbdev and DRM drivers without page flips use unsynchronized damage updates, so those fallbacks cannot guarantee tear-free output. Sustained 60 Hz also depends on the driver, display mode, hardware and application workload.

Backend selection:

```sh
# Automatic DRM selection, then fbdev fallback:
hoswm
# Select a DRM card (falls back to fbdev if initialization fails):
HOS_DRM_DEVICE=/dev/dri/card0 hoswm
# Force the framebuffer path:
HOS_FB_DEVICE=/dev/fb0 hoswm
```

Startup logs identify the selected backend, confirmed page-flip completions and hardware/software cursor support. The same selection applies to `hoswm --greeter`; the desktop hardware cursor is enabled after authentication.

Validation and measurement:

```sh
cargo test
cargo run --release --example render_bench
RUSTFLAGS='-C target-feature=+crt-static' cargo build --release --locked --offline
python3 tests/greeter_boot.py /path/to/vmlinuz /path/to/coreutils target/release/hoswm
HOS_TEST_DRM=1 python3 tests/greeter_boot.py /path/to/vmlinuz /path/to/coreutils target/release/hoswm
```

The CPU benchmark compares full composition with cached pointer movement for four 640×400 windows on an 800×600 canvas. It excludes framebuffer uploads, GPU/VM scheduling, refresh timing and input-to-photon latency; use it to track CPU regressions, not as a display FPS measurement. The QEMU tests cover keyboard input, window dragging and restored background pixels, login/logout and, on virtio, hardware cursor and page-flip activation.

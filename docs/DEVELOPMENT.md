# Build and development

Mochune is in early development. The source tree does not yet provide a one-command, clean-machine toolchain installer. The owner’s `run.sh` points to their external-SSD setup; change that setup or use Cargo directly on another machine.

## Linux

Prerequisites:

- Rust 1.92 or newer (the locked Slint version's minimum), Cargo, a native C/C++ toolchain and pkg-config.
- GStreamer 1.24 or newer development/runtime libraries, including app, video and OpenGL components.
- Runtime network, audio and codec plugins appropriate for your distribution; a working OpenGL/EGL driver.
- yt-dlp and Deno for YouTube extraction; FFmpeg for downloads that require merging.

With the native dependencies and helper locations configured:

```sh
cargo run --locked
```

Paste a YouTube URL or search, then select a result. The last mode is remembered; the header switches to independent audio-only Music mode.

## Linux rendering

Normal `./run.sh` and installed launches use software rendering in Music and the existing OpenGL renderer in Video. Switching modes re-executes the same executable after shutting down playback workers. A private, bounded metadata handoff restores both modes’ selections, queues, positions, repeat/shuffle state and window dimensions; images and media buffers are not transferred. Returning to a mode leaves it paused, as with normal switching.

No activation flag is required. `--start-music` and `--start-video` can choose the initial mode; fresh installs start in Music and the last mode is otherwise remembered. `--linux-opengl-ui` is a diagnostic override that retains OpenGL for both modes without renderer relaunches. Relaunch timing is printed to the terminal and recorded as `renderer_ui_ready` with diagnostic logging enabled; it measures the event loop after window show, not compositor presentation. Software rendering affects UI drawing only, not the hardware-only video decoding policy.

## Windows

Use an MSVC x64 Rust target and matching GStreamer SDK/runtime. This project also supports Linux-hosted cross-building with cargo-xwin, clang/lld and the Windows SDK. The current cross-build configuration lives on the owner's external drive. See [packaging](../packaging/README.md) for prerequisites and bundle layout.

The portable launcher is one `.exe` and extracts into a persistent `Mochune` directory beside itself. Runtime updates preserve its data subdirectory. Close the app and delete that directory to remove its local data. Run `rust-external python3 scripts/prepare-windows-helper.py` to prepare the checksum-pinned official unpacked yt-dlp dependency on the configured external SSD; this does not build a distribution. The package recipe requires the entire `windows/helpers/yt-dlp` tree, including `_internal`, rather than the standalone self-extracting helper.

## Checks

```sh
cargo fmt --check
cargo test --locked --lib
```

Use `./run.sh --doctor` to check the configured development environment. Validate playback on the target operating system; Wayland/X11, hardware decoding and native dialogs depend on installed libraries and drivers. Measurement conditions are documented in [Performance](PERFORMANCE.md).

## Contributions

Small, focused changes are welcome. Include the platform, reproduction steps and relevant checks in bug reports or pull requests. Keep credentials, cookies, watch history, generated packages and local caches out of Git. Preserve the memory-first design and third-party notices. Contributions to original application code use GPL-3.0-only unless explicitly identified otherwise.

Public binary release requirements are listed in the [packaging checklist](../packaging/README.md#release-checklist). Package creation and publishing remain explicit release steps.

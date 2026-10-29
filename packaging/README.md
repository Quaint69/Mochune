# Packaging

Package creation remains deferred until explicitly requested. Default invocations perform prerequisite checks only.

## Debian

`~/.local/bin/rust-external python3 scripts/package.py deb` checks the external GStreamer headers and local packaging tools. Add `--build` only when authorized. The recipe includes verified yt-dlp/Deno helpers and declares GStreamer core, OpenGL, playback, network, codec, hardware-decoder and audio plugin packages as dependencies. It reuses distribution libraries rather than shipping a second complete multimedia framework.

## Windows x64

The Rust target, cargo-xwin, Windows SDK/CRT, GStreamer 1.26.11 runtime/development SDK, app-local Microsoft runtime and extracted NSIS tools are on the external SSD. `GSTREAMER_1_0_ROOT_MSVC_X86_64` can override the default `windows/gstreamer/msvc_x86_64` SDK. Its pkg-config prefixes must point at its external host directory for Linux cross compilation.

`~/.local/bin/rust-external python3 scripts/package.py windows` checks prerequisites. With explicit authorization, `--build` creates `packages/Mochune-Windows-x64-portable.exe` on the external SSD. This is one self-extracting portable executable, with no installer, administrator access or registry registration. It creates a persistent `Mochune` directory beside the launcher. `Mochune/runtime` holds the app and dependencies; `Mochune/data` holds all app-owned settings, library, caches, helper scratch and native streaming temporary files. A small preparation banner appears only while extracting a new or changed runtime. A content-derived bundle identity allows later launches to skip both extraction and the banner. On updates only `runtime` is replaced; `data` is preserved. A per-folder file lock prevents concurrent launcher instances from modifying the runtime while it runs. Extraction/write failures are visible and never fall back to LocalAppData. Close the app before deleting the `Mochune` folder to remove its data; the launcher can create a fresh folder on its next launch. Earlier LocalAppData data is not imported or deleted automatically.

The bundler selects playback plugins and recursively resolves their DLL imports, including the scanner, TLS module and YouTube helpers. It fails on unresolved third-party DLLs. Required Microsoft runtime DLLs are loaded beside the app; no runtime installer runs. The bundle includes certificate authorities and dependency license texts. It excludes SDK headers, development archives, debug symbols and build caches. The launcher uses an 8 MiB LZMA dictionary rather than keeping its entire payload in RAM.

Cross-compilation and static import checks do not validate Windows playback. Test startup, search, music/video, quality changes, seeking, offline cached music, native dialogs and GPU decoding on an actual Windows PC. Windows N editions may need their operating-system Media Feature Pack for Media Foundation codecs. Graphics acceleration depends on the installed driver. The build is unsigned.

Windows x64 and Debian x64 are the project targets. Android support has been removed.

See the [user guide](../docs/USAGE.md) and the release checklist below.

The package is written to a `.building.exe` first and replaces the deliverable only after successful compression. `windows-build-report.json` records its bundle identity and sizes.

Windows helper preparation: `rust-external python3 scripts/prepare-windows-helper.py` fetches the pinned official yt-dlp onedir archive and verifies SHA-256 before extraction. The runtime carries `yt-dlp/yt-dlp.exe` and its complete `_internal` tree, with release provenance in `runtime-source.json`. Do not replace it with the one-file executable: that restores per-request temporary extraction. No app distribution is built by this preparation step.

## Windows per-user installer

Run `./pack-windows-installer.sh` to build `packages/Mochune-Windows-x64-setup.exe` on the external SSD. Installation needs no administrator access and defaults to `%LOCALAPPDATA%\Programs\Mochune`, with a directory picker. Desktop and Start Menu shortcuts are optional. These launch the app directly, without portable extraction or its preparation banner. Settings, likes, playlists, caches and editable themes use `%LOCALAPPDATA%\Mochune` and are erased during uninstall (including custom themes). Updates preserve them. Uninstall from Windows Installed apps or the Start Menu shortcut. Close Mochune before updating or uninstalling. Portable data remains separate and is not imported automatically.

The Debian package includes editable theme templates in `/usr/share/mochune/themes`, normalizes installed file permissions, and derives linked-library ABI requirements from the built executables. Upgrades preserve profiles. Removal erases standard Mochune and legacy app profile folders for local desktop accounts, as their own users; running installed instances must be closed first. Exported media downloads, development profiles on the external toolchain SSD and separate portable copies are not removed.

Installer artwork is generated from the existing app icon with `scripts/make-installer-art.py` (Pillow). It emits 2× static bitmaps and an RTF copy of the unchanged GPL text with explicit, high-contrast colors. `REVIEW_ONLY` builds a screenshot-only wizard whose install section and launch action cannot modify or start the installed application.

Every packaging path requires PastelDuck, PixelDuck and Phosphor CRT, and verifies declared resources before compiling. Missing themes fail the build rather than silently shipping an incomplete theme selection. Portable warm starts repair missing stock themes/fonts without replacing the existing runtime or user-edited files. The one-time duck-to-Mochune migration moves duck-era stock folders (identified by `walk.gif` or `ducklife.gif`) into `theme-backups/<bundle-id>/` before installing the replacement stock folders; this preserves any previous edits outside the active theme catalog. Portable backups live beside `themes/`; installed backups live in the per-user Mochune data folder.

## Release checklist

Before publishing binaries, retain the exact dependency versions, build configuration, checksums, required notices and corresponding sources/build instructions for applicable GPL/LGPL components. Preserve required replaceability/relinking rights for bundled libraries. The app license does not replace third-party terms; check redistribution rights for the Microsoft runtime and all bundled helpers, native plugins and fonts. See [NOTICE](../NOTICE) and [third-party materials](../third-party/).

Build every platform from the intended source revision, verify package contents and checksums, and test installation, updating, playback and uninstall on the target OS. Keep profiles, credentials, SDK archives, local logs and private test data out of published packages and source. Package creation and publication are explicit release steps.

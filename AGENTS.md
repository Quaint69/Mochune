# Project instructions

- Low memory footprint is the highest priority; speed is second. Prefer bounded work, small images, releasing inactive playback resources, and opt-in caches over speculative loading.

- The old Go/terminal implementation is abandoned. Work on the Rust/Slint source only.
- Do not create Windows, Debian, or Android distribution packages unless the owner explicitly requests package creation. Local development builds/checks are allowed.
- Keep toolchains, dependencies, intermediate/final builds, Rust temporary files and package staging on the configured external SSD. Use the guarded ~/.local/bin Rust commands; verify the drive UUID before storage changes.
- Sub-agents may inspect, investigate, analyze, and report only. They must not mutate files, data, configuration, Git history, or other state. The primary agent must independently verify their findings and perform edits itself. Do not spawn without explicit delegation authorization.

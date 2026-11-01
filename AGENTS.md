# Flacura: repository instructions

## Scope and product priorities

- Flacura is a small native audio player, one Rust 2024 binary crate. There is
  no web frontend, Node build, server, database or installer project.
- Windows x86_64 is the current development and release target. CI uses the
  MSVC toolchain on `windows-latest`. Linux and macOS builds are deferred.
- Retain existing platform-gated code; do not add Linux/macOS CI or packaging
  unless requested. Windows-only CI does not verify other platforms.
- Keep startup fast, playback responsive, the executable small and idle CPU
  usage low. Prefer the existing dependencies and module structure.

## Code map

| File | Responsibility |
| --- | --- |
| `src/main.rs` | CLI file argument, `--register`/`--unregister`, single-instance setup, starts playback before creating the window; `APP_TITLE` embeds `CARGO_PKG_VERSION`. |
| `src/audio.rs` | CPAL output, decoder thread, command/event channels, lock-free `rtrb` sample buffer, channel mapping, Rubato FFT resampling, seek/volume fades and output-device recovery. |
| `src/decode.rs` | Symphonia probing/decoding setup, metadata, quality labels and the shared `AUDIO_EXTENSIONS` list. |
| `src/waveform.rs` | Background peak/RMS waveform generation, cancellation, progressive atomic buckets; up to six workers for known-length files and one for unknown lengths. |
| `src/ui.rs` | eframe/egui window, custom drawing, keyboard/drop input, naturally sorted folder queue, transport, settings debounce and repaint scheduling. |
| `src/motion.rs` | FFT analysis of played stereo samples, loudness energy and onset pulses driving the background. |
| `src/aurora.rs` | OpenGL shader and color conversion for the music-driven aurora background. |
| `src/ipc.rs` | Per-user local IPC; subsequent launches forward a path or focus request to the running instance. Windows uses a named pipe. |
| `src/config.rs` | Small `key=value` settings file; Windows path is `%APPDATA%\Flacura\config.txt`. Stores slider position and mute. |
| `src/platform.rs` | File associations: Windows HKCU registration and Default Apps launch; platform-gated Linux/macOS implementations. |

## Important invariants

- Keep file I/O, decoding, resampling and expensive analysis off the CPAL
  output callback. Do not introduce blocking locks, sleeps, logging or heap
  allocation in its hot path. Feed it through the existing ring buffer.
- Preserve the flush request/acknowledgement protocol, command generations
  and pause generations in `audio.rs`. They prevent stale end-of-track events
  and slow loads from undoing newer user actions. Review atomic ordering when
  changing communication between threads.
- Prefer the source sample rate when the device supports it; resample only
  when needed. Preserve fades on seek, pause and volume changes.
- Waveform workers decode independently of playback. Cancel the old waveform
  on track changes and preserve the unknown-duration fallback.
- `Config.volume` stores the slider position, not linear gain. `volume_gain`
  in `ui.rs` applies the cubic gain curve and mute.
- Use the existing wake callbacks and repaint scheduling. Do not replace
  event-driven idle behavior with an always-running render/poll loop.
- Keep lossless gold / lossy blue styling and the existing hi-res definition:
  lossless with more than 16 bits or more than 48 kHz.
- When adding a format, review decoder support, `AUDIO_EXTENSIONS`, folder
  queue/drop behavior, platform associations and README together.
- Preserve shader attribution and the license notice in `aurora.rs`/README.

## Build and validation

Use stable Rust. Windows MSVC needs Visual Studio C++ Build Tools. Local GNU
Rust is also supported but needs MinGW-w64 binutils (`dlltool`) on PATH.

```text
cargo fmt --check
cargo clippy --locked --release -- -D warnings
cargo test --locked --release
cargo build --locked --release
```

The executable is `target/release/flacura.exe`. Release builds use fat LTO and
one codegen unit, so compilation can take several minutes. `cargo check
--locked` is useful for fast feedback but does not replace release validation
when changing Rust code. Keep `Cargo.lock` committed for reproducible builds.

- Tests are inline `#[cfg(test)]` modules. Existing tests cover IPC encoding,
  natural ordering, time formatting, color conversion and motion response.
- For Rust behavior changes, add focused regression tests where practical and
  run the relevant tests plus the CI checks. For documentation/workflow-only
  edits, validate links/configuration and run `actionlint` if available;
  rely on CI for the actual Windows build rather than adding redundant tests.
- Two ignored probes need real local music files. In PowerShell:

```powershell
$env:FLACURA_TEST_DIR = 'C:\path\to\music'
cargo test --locked --release probe_dir -- --ignored --nocapture
cargo test --locked --release probe_motion -- --ignored --nocapture
```

- These probes return without testing audio when the variable is unset. They
  are diagnostic tools, not automated playback coverage.
- CI does not verify audible output or interactive graphics. For playback/UI
  changes, manually check opening/dropping files, pause/resume, seek, next/
  previous, volume/mute, single-instance handoff and relevant error cases.
  Report checks that were not performed. File-association commands alter the
  user's registry/settings; do not run them as routine automated tests.
- Do not commit `target/`, local music, generated binaries or machine-local
  agent settings. Keep source/docs in English and follow existing rustfmt style.

## GitHub Actions and releases

- `releases` is triggered only by pushes to `main` (normally after merging).
  Branch pushes, PRs and tag pushes do not trigger builds. Do not add them back
  without a user request; this is primarily a personal Windows application.
- `build.yml` is reusable only. It checks formatting, Clippy, tests and release
  compilation once per main run, with `contents: read`, and exports its label.
- Development artifacts use `flacura-windows-x86_64-vVERSION-dev.COMMIT` and
  expire after 14 days. Releases attach `flacura-windows-x86_64-vVERSION.zip`
  containing `flacura.exe`; no installer is currently produced.
- Use Conventional Commits and preserve them in squash-merge titles: `fix:`
  bumps PATCH, `feat:` bumps MINOR, and `feat!:`/`BREAKING CHANGE:` bumps MAJOR.
  `docs:`, `ci:` and `chore:` alone do not request a release.
- Release Please uses `release-please-config.json` and
  `.release-please-manifest.json`. It creates a PR updating `Cargo.toml`,
  `Cargo.lock`, the manifest and `CHANGELOG.md`. The initial release is 0.1.0.
  Do not bump versions or create/push release tags for routine changes.
- On main, the bot first prepares/updates the release PR, or creates a release
  when that PR was merged. The build then uses the released SHA/tag if present,
  otherwise the triggering main SHA. A bot failure must not block the build.
- For a new release, `publish.yml` downloads the artifact from the same run,
  packages it and uploads the ZIP after the build passes. It never recompiles.
- Keep write permissions in the publishing/release workflows. A reusable
  workflow cannot request more permissions than its caller, even for skipped
  jobs. Do not put a `contents: write` job inside the read-only build workflow.
- The built-in `GITHUB_TOKEN` cannot trigger new workflows through bot-created
  PRs/tags. Keep the direct reusable workflow calls. The tag must match the
  Cargo version and lockfile; release artifacts must come from the tagged SHA.
- GitHub Actions must be allowed to create PRs in repository settings. Inspect
  the current setting if the bot fails; do not store personal tokens or assume
  the setting is enabled. Rerun failed publishing jobs to retry asset upload.
- Keep `CLAUDE.md` as a thin import of this file so instructions do not diverge.

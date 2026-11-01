# Flacura

An extremely fast, minimal audio player, currently built and released for
Windows (x86_64). Linux and macOS code remains in the repository, but builds
and releases for those platforms are deferred until needed.

- **Starts instantly.** Audio starts about 90 ms after launch, before the window
  is even on screen. If Flacura is already running, opening another file hands
  it over to the existing window in a few milliseconds.
- **A background that follows the music.** A faint aurora glows behind the
  player (gold for lossless, blue for lossy) and follows the loudness of what
  is playing: quiet passages leave it low, dim and nearly still, loud ones
  make it brighter, reach further down and flow several times faster, and
  beats give it a short flash. It stays out from behind the title and the
  controls, so they remain readable. When paused it comes to rest.
- **Waveform seekbar.** The whole track is drawn as a waveform that grows in
  while it's decoded in the background, on several threads (a 3-minute FLAC
  takes about 35 ms). Click or drag to seek.
- **Transport and volume.** Stop, previous, play/pause and next buttons, a
  volume slider and a mute button. Volume and mute are remembered.
- **Quality at a glance.** Codec, bit depth and sample rate, or bitrate for
  lossy files. Lossless tracks are highlighted in **gold**, and hi-res
  (more than 16 bits or more than 48 kHz) gets a glowing `HI-RES` badge.
- **Folder queue.** The other audio files in the same folder play next, in
  natural order (`2` comes before `10`). Unplayable files are skipped.
- **Native rate output.** When the device supports the file's sample rate, the
  device is opened at that rate and nothing is resampled. Otherwise Flacura
  resamples with a high-quality FFT resampler. Seeking and track changes fade
  out in a few milliseconds instead of clicking.
- One small self-contained binary (~8 MB), with no runtime to install.

Supported formats: FLAC, MP3, WAV, AIFF, ALAC, AAC (M4A/MP4), Ogg Vorbis,
CAF, MKV/WebM audio. Opus, WavPack and APE are not supported yet (no pure-Rust
decoders).

## Controls

| Input                          | Action                                   |
|--------------------------------|------------------------------------------|
| Space / play button / click background | Play / pause (replay at the end) |
| S / stop button                | Stop and rewind to the start             |
| N, Page Down / next button     | Next track in the folder                 |
| P, Page Up / previous button   | Previous track (restarts if past 3 s)    |
| Click / drag waveform          | Seek                                     |
| ← / →                          | Seek 5 s (Shift: 30 s)                   |
| ↑ / ↓, mouse wheel, slider     | Volume                                   |
| M / speaker button             | Mute / unmute                            |
| Drag & drop                    | Play the dropped file                    |

## Settings

Volume and mute are saved automatically to:

- Windows: `%APPDATA%\Flacura\config.txt`
- Linux: `$XDG_CONFIG_HOME/flacura/config.txt` (usually `~/.config/flacura/config.txt`)
- macOS: `~/Library/Application Support/Flacura/config.txt`

## Make it the default player

```bash
flacura --register
```

- **Windows:** registers Flacura for audio files under `HKCU` (no admin
  needed) and opens *Settings > Default apps*, where you pick Flacura.
  Windows doesn't let apps set themselves as the default.
- **Linux:** installs `~/.local/share/applications/flacura.desktop` and sets it
  as the default for common audio MIME types via `xdg-mime`.
- **macOS:** Finder integration needs an app bundle, which isn't provided yet.
  Run `flacura file.flac` or drop files on the window.

`flacura --unregister` removes those entries.

On Linux, Flacura uses XWayland when it is available, because native Wayland
windows from winit can't receive dropped files and get no title bar. Set
`FLACURA_WAYLAND=1` to run as a native Wayland client anyway.

## Building

You need a stable Rust toolchain (<https://rustup.rs>).

```bash
cargo build --release
```

The binary is in `target/release/`.

**Linux** also needs pkg-config and the ALSA and windowing headers:

```bash
sudo apt install pkg-config libasound2-dev libxkbcommon-dev libwayland-dev libgl1-mesa-dev
```

**Windows** works with either toolchain:

- MSVC (`x86_64-pc-windows-msvc`): install *Visual Studio Build Tools* with
  the C++ workload.
- GNU (`x86_64-pc-windows-gnu`): needs MinGW-w64 binutils (`dlltool`) on
  `PATH`, e.g. `winget install BrechtSanders.WinLibs.POSIX.UCRT`.

### Automatic Windows releases

Merge changes into `main`. GitHub checks the code, builds the Windows x86_64
application once, and publishes **flacura.exe** directly to
[GitHub Releases](https://github.com/kratagit/flacura/releases/latest).
Download the EXE and run it; no ZIP extraction is needed.

Branch pushes and PRs do not start builds. There is no separate release PR,
manual tag or version-editing step. A failed check prevents publication.

Each release automatically increments the patch version (`0.1.0` → `0.1.1`
→ `0.1.2`). The build embeds that version in the window title. Cargo.toml is
used as a minimum version baseline; CI updates its local copy and Cargo.lock
before compiling without creating additional commits. Retrying a tagged
commit reuses its release version. Release tags point to the source commit.

If a run fails, use **Re-run failed jobs** in Actions. No personal access token
or permission to create pull requests is needed.

### Tests

```bash
cargo test
# Print quality info and waveform timing for every file in a directory:
FLACURA_TEST_DIR=/path/to/music cargo test --release probe_dir -- --ignored --nocapture
```

## Coding agent instructions

See [AGENTS.md](AGENTS.md) for the project architecture, development commands,
performance constraints and release process. [CLAUDE.md](CLAUDE.md) imports the
same instructions for Claude Code.

## How it works

```
main ──► single-instance check (per-user named pipe / socket) ──► forward path & exit
  │
  ├─ decoder thread: symphonia ─► channel map ─► (rubato FFT resampler) ─► lock-free ring
  │                                                                          │
  │                                              cpal device callback ◄─────┘
  │                                              (volume/pause/seek fades, stereo tap)
  ├─ waveform threads: each decodes one slice of the file ─► atomic buckets
  └─ UI (egui + OpenGL): aurora background shader driven by loudness and
     beats (realfft onsets) from the stereo tap, waveform, transport, badge.
     Capped at 60 fps, no redraws at all once paused.
```

## Credits

The background is ported from the "Aurora" background of
[React Bits](https://reactbits.dev/c/backgrounds/aurora) (MIT + Commons Clause).

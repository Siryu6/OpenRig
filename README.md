# OpenRig

Open source guitar multi-effects pedalboard in Rust for the Raspberry Pi 5
(and x86_64 mini PCs) with a touchscreen: LV2 effects hosted through lilv,
neural amp captures (NAM), impulse responses, real-time audio with JACK, a
native Slint UI, MIDI, and optional footswitches and encoder on GPIO. It
ships as a bootable OS image with A/B updates.

<p align="center">
  <img src="docs/media/demo.gif" width="800"
       alt="OpenRig on a 1024x600 screen: rack, knob strip, block editor, presets and the Live page">
</p>
<p align="center"><sub>
  The x86_64 image running in QEMU at 1024x600. Captures from
  <a href="https://www.tone3000.com">TONE3000</a>: Boss MT-2 by @kosmicaudio,
  Peavey 5150 by @doubleyoud, BOSS CE-2w by @matteocapuni, Orange and Mesa
  cabinets by @tonefactor.
</sub></p>

- **Rack**: the signal flow on one screen, serial blocks plus A/B, Mix and
  left/right splits. Tap a block to get its knobs at the bottom of the
  screen, double tap for the full editor, long press to bypass.
- **Amps, pedals, cabinets, rooms**: Neural Amp Modeler captures and impulse
  responses as built-in blocks, next to any LV2 plugin.
- **TONE3000**: link your account and download captures right on the
  pedalboard (favorites, your own tones, trending), with credits and
  licenses kept.
- **Live**: 8 scenes per preset, setlists, stomp mode, tap tempo with the
  tempo sent to plugins, tuner, looper.
- **MIDI**: preset, scene and stomp control with a Helix-style map, clock
  and CC learn.
- **Appliance OS**: a Buildroot image that boots straight into the UI, with
  a read-only system, a data partition and A/B updates with rollback.

It runs on a Raspberry Pi 5 with a USB audio interface and a 10" touch
screen. It is a young project: expect rough edges.

## Features

- **LV2 hosting**: plugins are discovered with lilv (`LV2_PATH` or the
  standard locations). Controls, ranges, units, toggles, integers and
  enumerations are read from the plugin metadata and edited in the UI. URID
  map / unmap, options, bounded-block-length and isLive are provided; plugins
  that need anything else are skipped. Atom inputs get the tempo
  (`time:Position`) and MIDI. Without any installed plugin, a demo
  catalogue (passthrough) keeps the UI usable for development.
- **Amp, Amp + Cab and Pedal blocks** (feature `nam`, default): built-in neural blocks
  running Neural Amp Modeler `.nam` captures (and RTNeural `.json` /
  `.aidax` models) through [NeuralAudio](https://github.com/mikeoliphant/NeuralAudio).
  The editor lists tones (a folder of captures, e.g. one TONE3000 tone)
  rather than files: choosing a tone loads its first preset, and a drop-down
  switches between its presets. Each block only lists the tones of its
  gear (Amp: heads; Amp + Cab: combos and amps with their cab;
  Pedal: pedals, outboard, experimental). Each has a noise gate (as in the NAM plugin: it listens to
  the clean input and gates the model output); the amp blocks add a bass /
  middle / treble tone stack, and packed A2 captures a Full / Light quality
  (Light is about four times cheaper). Saved in presets. See [docs/NEURAL_MODELS.md](docs/NEURAL_MODELS.md).
- **Cabinet and Space blocks**: impulse response loaders (`.wav`) for
  cabinets and rooms, with level, dry/wet mix (35 % by default on Space),
  loudness normalization (unity gain over the guitar band, 80 Hz – 5 kHz) and low / high cut. Zero latency; the tail of long room responses is
  convolved on a worker thread so the audio callback cost stays flat.
- **Wi-Fi** in Settings: scan, join (on-screen keyboard with symbols, hidden
  password), disconnect, forget, radio on/off. It drives NetworkManager
  through `nmcli` (as on Raspberry Pi OS); elsewhere (macOS, Windows, Linux
  without NetworkManager) Settings says Wi-Fi is managed by the system.
- **Power off / restart** from the menu ⋮ on the OS image (the launcher sets
  `PEDALBOARD_POWER_REQUEST`; the app saves the session and quits, the
  launcher switches the board off). Hidden on a desktop.
- **TONE3000 Connect** (feature `tone3000`, default): Settings → TONE3000
  Connect links a [tone3000.com](https://www.tone3000.com) account with the
  OAuth device flow (QR code and short code on the screen, approved on a
  phone). Each user enters the *publishable* key (`t3k_pub_…`) of their own
  TONE3000 application; no key ships with the code. Once connected, the
  TONE3000 features are enabled (`Features.tone3000` flag in the UI): the
  **TONE3000 library** (menu ⋮, or the TONE3000 button of a block)
  lists your favorites, your own and downloaded tones, and the trending /
  latest ones, by gear. Models are downloaded one at a time
  when you ask, into `models/Tone3000/<gear>/<tone>/` with the creator,
  license and link in `tone.json`, and can be loaded straight into the block.
  Opened from a block, the library only shows the gears that block plays;
  from the menu, "Add" puts a downloaded model in a new block of its type.
- **Chain with splits**: serial chain plus nested A/B, Mix and L/R splits;
  a split's type can be changed from the rack (its blocks are kept).
- **Stereo**: two outputs. The guitar comes in mono and becomes stereo where
  something makes it so: an L/R split (top branch to the left output, bottom
  branch to the right one — e.g. front of house with a cabinet IR on one
  side, a real cab without IR on the other), a Mix split with its branches
  panned (Pan button, stereo mode: each branch from hard left to hard right,
  centred by default), a plugin with two outputs or a stereo impulse
  response. After that point, cabinet / space blocks and mono
  LV2 plugins run once per side; NAM blocks (too costly to double) play the
  mix of both sides. Mono mode (rack toggle, saved in presets) keeps the whole
  path mono and sends the same signal to both outputs. The looper records in
  stereo. In the rack a single line carries a mono signal and a double line
  a stereo one.
- **Touch rack**: blocks are drawn after their gear (flat amp head, combo,
  cabinet with speakers, pedal with footswitch, reverb pill; LV2 plugins keep
  a plain card), coloured by family (amp red, cab orange, drive yellow,
  reverb teal, delay green, modulation blue…) or with a colour picked in the
  block editor. The UI is styled like dark, minimal hardware: near-square
  corners, amber accent, key-like buttons with status LEDs, recessed displays,
  input / output LED meters with clip LEDs on the rack, knobs tinted with the block's colour, LED
  tuner ramp, footswitch tiles on the Live page. Tap to select:
  the block's controls show as knobs at the bottom of the rack (five per page)
  for quick tweaks; double tap opens the full editor. Hold (0.35 s) and
  release in place to bypass, hold and drag to move a block anywhere,
  including into a split branch. A quick swipe scrolls the rack.
- **Real-time safe audio**: chains are built off the audio thread and swapped
  with a 20 ms equal-power crossfade. Parameters, bypass (10 ms ramp), A/B and
  mix changes are applied live without rebuilding. The JACK callback does no
  allocation and takes no blocking lock.
- **Presets, scenes and setlists**: presets store the whole chain (splits,
  parameter values, block colours) with 8 scenes and a tempo. A scene
  recalls bypass, parameters and split settings instantly, without
  rebuilding the chain; a setlist maps footswitch slots to presets, all
  preloaded in memory.
- **Live page**: 8 switch tiles in Stomp (block on/off, assignable),
  Scenes or Setlist mode, tempo display with TAP. The last footswitch taps
  the tempo and, held, opens the tuner. The tempo drives the BPM ports of
  LV2 plugins.
- **Tails (spillover)**: delays and reverbs keep ringing when bypassed or
  when the scene changes.
- **Session**: the current chain is saved a few seconds after each change (and on exit)
  and restored at startup, so switching the pedalboard off is safe.
- **Tuner** down to 38 Hz (7- and 8-string drop tunings, bass; analysis only while the tuner page is open, optional mute) and
  **looper** (record, overdub layers, undo, 30 s max) after the effects.
- **Resource monitor**: the top bar shows the DSP load (time spent in the
  audio callback vs. the buffer period) and the SoC temperature, coloured
  green / orange / red. Tapping it opens the details: DSP average and peak,
  JACK load, CPU (total and busiest core), memory, dropouts (xruns), latency
  and, on the Pi, under-voltage / throttling. Adding an effect while the load
  is already high shows a warning. The block editor shows the share of a
  CPU core the block uses.
- **MIDI** (feature `midi`, default): every USB / DIN MIDI input is heard,
  hot-plugged. Helix-compatible map: Program Change and CC0 banks for
  presets (or setlist slots), CC69 scenes, CC64 tap, CC68 tuner, CC49–56
  footswitches, CC60–63 looper; MIDI clock sets the tempo; MIDI learn on any
  block parameter or on / off (expression pedals) with a heel / toe range,
  saved with the preset; notes and free CCs go to LV2 plugins taking MIDI.
  See [docs/MIDI.md](docs/MIDI.md).
- **Hardware** (feature `gpio`): footswitches follow the Live page mode
  (act when pressed; the tap switch also reads the hold), and keys `1`–`8`
  and `t` do the same on a keyboard; the rotary encoder drives the UI like
  the arrow and Return keys.
- **OS image** (`os/`): Buildroot image for the Pi 5 and x86_64 mini PCs,
  `PREEMPT_RT` kernel, read-only system, boot logo, the UI full screen
  without X11 / Wayland, signed A/B updates (RAUC) that roll back when the
  new system does not start. See [docs/OS.md](docs/OS.md).

## Build

Dependencies (Debian / Raspberry Pi OS):

```bash
sudo apt install -y build-essential cmake pkg-config libjack-jackd2-dev liblilv-dev lv2-dev \
    libasound2-dev libfontconfig1-dev libxkbcommon-dev
# for --features kms (full screen without X11 / Wayland):
sudo apt install -y libinput-dev libgbm-dev libegl-dev libudev-dev
```

NeuralAudio is a git submodule (with its own nested submodules):

```bash
git submodule update --init --recursive
```

```bash
cargo build --release                    # LV2 hosting + NAM block + TONE3000 + MIDI (default)
cargo build --release --features gpio    # + footswitches / encoder on the Pi
cargo build --no-default-features --features winit,lv2,midi   # without NAM nor TONE3000 (no cmake / C++)
cargo build --no-default-features --features winit,lv2,nam,midi   # without TONE3000 Connect (no HTTPS client)
cargo build --no-default-features        # no liblilv, NAM, MIDI nor display: demo plugins, tests
cargo build --release --features kms     # + full screen without X11 / Wayland (libinput, libgbm, EGL)
cargo build --release --no-default-features --features kms,lv2,nam,tone3000,midi,gpio,splash   # as in the Pi image
cargo test
```

The NAM code is always compiled optimized (C++20, CMake), even in debug
builds, otherwise the models cannot keep up in real time.

The display comes from `winit` (a desktop window, default) or `kms` (full
screen on the display, no X11 / Wayland; the OS image uses it alone). With
neither, the app builds but cannot open its window. To build the bootable
image for the Pi 5 or a mini PC: `make -C os rpi5`, see
[docs/OS.md](docs/OS.md).

`tests/lv2_hosting_test.rs` compiles a small LV2 gain plugin with the system C
compiler and runs it through the host, so real hosting is tested without any
plugin installed.

## Run

```bash
jackd -d alsa -r 48000 -p 128 -n 2 &   # or PipeWire's JACK
cargo run --release
```

`pedalboard:guitar_in` is connected to the first capture port,
`pedalboard:out_left` and `pedalboard:out_right` to the first two playback
ports (only the left one with a single playback port: use the mono mode). Without JACK, the UI
still starts with audio disabled.

| Variable | Default | Purpose |
|---|---|---|
| `PEDALBOARD_DATA_DIR` | platform data dir (`~/.local/share/pedalboard` on Linux) | Presets, setlists, session, `models/` (NAM models and IRs) |
| `PEDALBOARD_TONE3000_CLIENT_ID` | unset | Prefills the TONE3000 publishable key (`t3k_pub_…`) when none is saved |
| `PEDALBOARD_NO_AUTOCONNECT` | unset | Do not connect JACK ports |
| `PEDALBOARD_FOOTSWITCH_PINS` | `17,27,22,23` | Footswitch GPIOs (BCM) |
| `PEDALBOARD_ENCODER_PINS` | `5,6,13` | Encoder A, B, button (empty: none) |
| `PEDALBOARD_ENCODER_STEPS` | `4` | Quadrature steps per detent |
| `LV2_PATH` | system | Where LV2 bundles are searched |
| `RUST_LOG` | `info` | Log level |
| `PEDALBOARD_EXIT_ON_AUDIO_CHANGE` | unset | Quit when the JACK server stops or starts (the OS launcher starts the UI again) |
| `PEDALBOARD_POWER_REQUEST` | unset | File where Power off / Restart is requested (set by the OS launcher; shows the menu entry) |

Switches and encoder are wired between the GPIO and ground (internal pull-ups).

Guitar-oriented LV2 packages: `guitarix-lv2`, `x42-plugins`, `calf-plugins`,
`dragonfly-reverb-lv2`, `lsp-plugins-lv2`.

## Code layout

```
src/
├── main.rs              UI wiring (callbacks, polling and monitor timers)
├── bin/splash.rs        Boot logo on the Linux framebuffer (feature splash, OS image)
├── gpio.rs              Footswitches and encoder (feature gpio)
├── live_ui.rs           Live page: footswitch modes, tap tempo, hold → tuner
├── midi_io.rs           MIDI inputs (midir, feature midi): hot-plug, clock → tempo
├── midi_ui.rs           MIDI actions, learn in the block editor, Settings → MIDI
├── tone3000/            TONE3000: account (device flow, tokens), library (lists, downloads)
├── tone3000_ui.rs       Settings → TONE3000 Connect, library page, Features.tone3000 flag
├── wifi.rs              Wi-Fi through NetworkManager (nmcli): scan, join, forget
├── wifi_ui.rs           Settings → Wi-Fi (worker thread running nmcli)
├── monitor.rs           DSP meter, xruns, CPU / RAM / temperature / throttling
├── audio/
│   ├── jack_client.rs   JACK callback: tuner → effects → looper
│   ├── chain_switcher.rs  Builder thread, crossfade, live commands
│   ├── engine.rs        Audio engine handle
│   ├── split_chain.rs   ChainNode / SplitType (chain description)
│   ├── tuner.rs, looper.rs
├── plugins/
│   ├── lv2_host.rs      lilv discovery and instantiation
│   ├── lv2_features.rs  URID map/unmap, options
│   ├── atom.rs          Atom input events: transport (time:Position) and MIDI
│   ├── instance.rs      Running plugin with pre-connected buffers
│   ├── dsp.rs           Small DSP blocks: cuts, noise gate, tone stack
│   ├── chain.rs         Real-time chain (plugins + splits), ChainSpec/ChainCommand
│   ├── nam.rs           NAM model loading and processing (FFI to native/)
│   ├── ir.rs            Impulse responses: WAV loading, resampling, convolution
│   └── scanner.rs       Plugin catalogue (+ built-in and demo blocks)
├── midi.rs              MIDI parsing, the Helix-style map, clock tracker, heel / toe ranges
├── state/               AppState (single source of truth for the chain, presets,
│                        setlists, session), scenes of a preset, tap tempo, learned CCs
├── presets/             Preset and setlist storage (JSON files written atomically;
│                        setlists are `Scene` there)
└── ui/                  Chain layout (block positions, connectors), view models, text editing
native/                  C wrapper around NeuralAudio, built with CMake by build.rs
third_party/NeuralAudio  Submodule: NAM Core + RTNeural
third_party/lilv, i-slint-backend-linuxkms   Patched crates (see their PATCHED.md)
ui/                      Slint UI (app.slint, types.slint, pages/, components/, theme.slint)
os/                      Buildroot external tree: Pi 5 / x86_64 images, RAUC updates, init scripts
docs/                    OS image, live (scenes, setlists, tempo), MIDI, neural amp models (NAM)
```

## Architecture

Three threads share the chain:

- **UI thread** (Slint): every edit goes through `AppState`, the single
  source of truth. Structural changes (add, remove, move, preset) produce a
  `ChainSpec`; live changes (parameter, bypass, A/B, mix) produce a
  `ChainCommand`. MIDI threads (one per input) hand messages to the UI
  thread and, for LV2 plugins taking MIDI, straight to the audio thread.
- **Chain builder thread**: instantiates the plugins of a `ChainSpec` and
  hands the finished chain to the audio thread, with the live commands
  received meanwhile replayed on it.
- **JACK callback**: tuner → effects → looper, with preallocated buffers,
  `try_lock` only, and atomics for the DSP meter. A new chain is crossfaded in;
  the old one is dropped off the audio thread.

## Docs

- [docs/OS.md](docs/OS.md): the pedalboard OS image (Raspberry Pi 5, x86_64
  mini PC): build, flash, settings, SSH, storage, boot logo, A/B updates
- [docs/SCENES.md](docs/SCENES.md): scenes, setlists, footswitch modes, tempo and tails
- [docs/MIDI.md](docs/MIDI.md): the MIDI map, learn and ranges, clock, MIDI to plugins
- [docs/NEURAL_MODELS.md](docs/NEURAL_MODELS.md): the Amp / Amp + Cab / Pedal / Cabinet / Space blocks, Tone3000, roadmap
- [PROJECT_STATUS.md](PROJECT_STATUS.md): what works, what is left

## License

GPL-3.0-or-later (see [LICENSE](LICENSE)). The UI toolkit,
[Slint](https://slint.dev), is used under its GPL-3.0 license (its
royalty-free license does not cover embedded devices). Bundled third-party
code keeps its own license: NeuralAudio (MIT, with NAM Core and RTNeural),
the patched lilv bindings (MIT) and the patched Slint linuxkms backend
(GPL-3.0) in `third_party/`.

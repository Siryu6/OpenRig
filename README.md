<h1 align="center">OpenRig</h1>

<p align="center">
  <b>The open source guitar pedalboard you build yourself.</b><br>
  Raspberry Pi 5 or x86_64 mini PC, a USB audio interface and a touchscreen.
</p>

<p align="center">
  <a href="https://openrig.eu">Website</a> ·
  <a href="https://docs.openrig.eu/">User guide</a> ·
  <a href="https://docs.openrig.eu/getting-started/hardware/">Build one</a> ·
  <a href="https://github.com/Siryu6/OpenRig/releases/latest">Download</a>
</p>

<p align="center">
  <a href="https://github.com/Siryu6/OpenRig/releases/latest"><img alt="Latest release" src="https://img.shields.io/github/v/release/Siryu6/OpenRig?color=f0a43c"></a>
  <a href="https://github.com/Siryu6/OpenRig/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/Siryu6/OpenRig/actions/workflows/ci.yml/badge.svg"></a>
  <a href="LICENSE"><img alt="License: GPL-3.0-or-later" src="https://img.shields.io/badge/license-GPL--3.0--or--later-blue"></a>
</p>

<p align="center">
  <img src="docs/media/demo.gif" width="800"
       alt="OpenRig on a 1024x600 screen: rack, knob strip, block editor, TONE3000 library, splits, presets, the Live page, tuner, looper and settings">
</p>
<p align="center"><sub>
  The x86_64 image running in QEMU at 1024x600. Captures from
  <a href="https://www.tone3000.com">TONE3000</a>: Boss MT-2 by @kosmicaudio,
  Peavey 5150 by @doubleyoud, BOSS CE-2w by @matteocapuni, Orange and Mesa
  cabinets by @tonefactor; the library shows the TONE3000 trending tones,
  with their creators.
</sub></p>

OpenRig turns a small computer into a multi-effects pedalboard. It ships as
a bootable OS image that starts straight into a touch UI, runs effects in
real time with JACK, and updates itself without risking the board.

- **[Rack](https://docs.openrig.eu/using/rack/)**: the whole signal flow on one screen, serial
  blocks plus A/B, Mix and left/right splits, mono or stereo.
- **[Neural amps, pedals and cabinets](https://docs.openrig.eu/using/neural-blocks/)**: Neural
  Amp Modeler captures and impulse responses as built-in blocks, next to any
  compatible [LV2 plugin](https://docs.openrig.eu/setup/lv2/).
- **[Plugin store](https://docs.openrig.eu/setup/store/)**: free LV2 plugins built for the
  board, installed in one tap from a signed catalog; your own bundles still load.
- **[TONE3000](https://docs.openrig.eu/using/tone3000/)**: link your account and download
  captures right on the pedalboard, with credits and licenses kept.
- **[Live](https://docs.openrig.eu/using/live/)**: 8 scenes per preset, setlists, stomp mode,
  tap tempo, [tuner and looper](https://docs.openrig.eu/using/tuner-looper/).
- **[Inputs and outputs](https://docs.openrig.eu/setup/routing/)**: every input and output of
  the interface, with Send and Return blocks anywhere in the rack: a dry signal for reamping, a
  bass DI, a separate monitor mix.
- **[MIDI](https://docs.openrig.eu/using/midi/)**: Helix-style map for presets, scenes and
  stomps, clock and CC learn.
- **[Updates](https://docs.openrig.eu/setup/updates/)**: a read-only system with signed A/B
  updates that roll back when the new system does not start.

It is a young project: expect rough edges, and
[tell us](https://github.com/Siryu6/OpenRig/issues) when you hit one.

## Get started

1. **[Gather the hardware](https://docs.openrig.eu/getting-started/hardware/)**: a Raspberry Pi 5
   (or an x86_64 mini PC with AVX2, e.g. an Intel N100), a class-compliant USB audio interface, a
   touchscreen and a micro SD card. Footswitches and a MIDI pedal are optional.
2. **Write the image** from the [latest release](https://github.com/Siryu6/OpenRig/releases/latest):
   [Raspberry Pi 5](https://docs.openrig.eu/getting-started/raspberry-pi/) or
   [x86_64 mini PC](https://docs.openrig.eu/getting-started/mini-pc/).
3. **[First start](https://docs.openrig.eu/getting-started/first-start/)**: levels, Wi-Fi,
   TONE3000 and your first sound.

Then the [tour of the screen](https://docs.openrig.eu/using/tour/) shows where everything is.
When something goes wrong, see [Troubleshooting](https://docs.openrig.eu/help/troubleshooting/)
and the [FAQ](https://docs.openrig.eu/help/faq/).

To try the app on a Linux desktop without the image, the release also holds
`openrig-<version>-linux-x86_64.tar.gz` (and `aarch64`): it needs JACK and
lilv (`apt install jackd2 liblilv-0-0`); start JACK, then `./pedalboard`.

## User guide

| | |
|---|---|
| **Getting started** | [Hardware](https://docs.openrig.eu/getting-started/hardware/) · [Raspberry Pi 5](https://docs.openrig.eu/getting-started/raspberry-pi/) · [Mini PC](https://docs.openrig.eu/getting-started/mini-pc/) · [First start](https://docs.openrig.eu/getting-started/first-start/) |
| **Playing** | [Tour](https://docs.openrig.eu/using/tour/) · [Rack](https://docs.openrig.eu/using/rack/) · [Block editor](https://docs.openrig.eu/using/editor/) · [Neural blocks](https://docs.openrig.eu/using/neural-blocks/) · [TONE3000](https://docs.openrig.eu/using/tone3000/) · [Presets, scenes and setlists](https://docs.openrig.eu/using/presets/) · [Live](https://docs.openrig.eu/using/live/) · [Tuner and looper](https://docs.openrig.eu/using/tuner-looper/) · [MIDI](https://docs.openrig.eu/using/midi/) |
| **Setting up** | [Settings](https://docs.openrig.eu/setup/settings/) · [Inputs and outputs](https://docs.openrig.eu/setup/routing/) · [Updates](https://docs.openrig.eu/setup/updates/) · [Footswitches](https://docs.openrig.eu/setup/footswitches/) · [LV2 plugins](https://docs.openrig.eu/setup/lv2/) · [pedalboard.txt](https://docs.openrig.eu/setup/pedalboard-txt/) · [Performance](https://docs.openrig.eu/setup/performance/) |
| **Help** | [Troubleshooting](https://docs.openrig.eu/help/troubleshooting/) · [SSH access](https://docs.openrig.eu/help/ssh/) · [FAQ](https://docs.openrig.eu/help/faq/) |

The guide is built from [Siryu6/openrig-doc](https://github.com/Siryu6/openrig-doc):
fixes are welcome there.

## Build from source

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
cargo build --release --no-default-features --features kms,lv2,nam,tone3000,midi,gpio,splash,update,store   # as in the Pi image
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

The pedalboard has as many inputs and outputs as the interface (at least one
input and two outputs, at most 16 each): `pedalboard:in_N` is connected to
capture port N and `pedalboard:out_N` to playback port N. Input 1 feeds the
chain and outputs 1 and 2 play its left and right sides, unless
`routing.json` in the data directory says otherwise (which input feeds the
chain; for each output or pair: the main mix, one of four sends or an input
passed through, level, balance, mute, solo, polarity, and how a stereo
source plays on a single output). With a single playback port, both sides
are summed on it. The outputs are set in Settings → Inputs and outputs →
Routing. In the rack, a Send block copies the signal at its place to a send
and a Return block brings an input in; see [docs/ROUTING.md](docs/ROUTING.md).
`in_1`, `out_1` and `out_2` keep their former names as aliases
(`guitar_in`, `out_left`, `out_right`). Without JACK, the UI still starts
with audio disabled.

| Variable | Default | Purpose |
|---|---|---|
| `PEDALBOARD_DATA_DIR` | platform data dir (`~/.local/share/pedalboard` on Linux) | Presets, setlists, session, `models/` (NAM models and IRs) |
| `PEDALBOARD_TONE3000_CLIENT_ID` | unset | Uses the publishable key (`t3k_pub_…`) of your own TONE3000 application instead of OpenRig's, when none is saved |
| `OPENRIG_TONE3000_CLIENT_ID` | OpenRig's key | At build time: the publishable key built in |
| `PEDALBOARD_NO_AUTOCONNECT` | unset | Do not connect JACK ports |
| `PEDALBOARD_FOOTSWITCH_PINS` | `17,27,22,23` | Footswitch GPIOs (BCM), until set in Settings → Hardware (empty: none) |
| `PEDALBOARD_ENCODER_PINS` | `5,6,13` | Encoder A, B and button, until set in Settings (`A,B`: no button; empty: no encoder) |
| `PEDALBOARD_ENCODER_STEPS` | `4` | Quadrature steps per detent (1, 2 or 4), until set in Settings |
| `PEDALBOARD_I2C_ENCODERS` | unset | External encoder boards on I2C (`seesaw`, `seesaw-quad`, `m5-stack8`), until set in Settings |
| `PEDALBOARD_UPDATE_URL` | GitHub's latest release | Release read by "Check for updates", to test with another server (plain HTTP allowed) |
| `LV2_PATH` | system | Where LV2 bundles are searched |
| `RUST_LOG` | `info` | Log level |
| `PEDALBOARD_EXIT_ON_AUDIO_CHANGE` | unset | Quit when the JACK server stops or starts (the OS launcher starts the UI again) |
| `PEDALBOARD_POWER_REQUEST` | unset | File where Power off / Restart is requested (set by the OS launcher; shows the menu entry) |
| `PEDALBOARD_ALIVE_FILE` | unset | File written again every second while the UI runs (set by the OS launcher, to tell an app update that hangs) |

Switches and encoder are wired between the GPIO and ground (internal
pull-ups); the GPIOs are set in Settings → Hardware. External encoder
boards go on the I2C bus (SDA GPIO 2, SCL GPIO 3, 3.3 V), enabled in the
OS image.

Do not wire switches to the GPIOs an audio HAT uses. 2-channel I2S HATs
take GPIO 18–21. 8-channel HATs (HiFiBerry DAC8x, RaspiAudio 8x8) take
GPIO 18–27, and GPIO 5 to detect their inputs. Those HATs overlap the
default footswitches (27, 22, 23) and encoder (5). OpenRig asks the
kernel which GPIOs a driver holds and leaves those inputs out; Settings →
Hardware says which. With an 8-channel HAT, GPIO 4, 6, 12, 13, 16 and 17
stay free (7–11 too when SPI is off).

Guitar-oriented LV2 packages: `guitarix-lv2`, `x42-plugins`, `calf-plugins`,
`dragonfly-reverb-lv2`, `lsp-plugins-lv2`.

## Releases

Each [release](https://github.com/Siryu6/OpenRig/releases) ships:

- `openrig-<version>-linux-x86_64.tar.gz` and `…-linux-aarch64.tar.gz`:
  the app, built with the default features. It needs JACK and lilv from the
  system (`apt install jackd2 liblilv-0-0`).
- `openrig-os-<version>-rpi5.img.xz` and `…-x86_64.img.xz`: the OS images,
  to write to a card or a disk (Raspberry Pi Imager, `dd`, balenaEtcher).
- `openrig-os-<version>-<board>.raucb` and `…-<board>-app.raucb`: the
  signed system and app updates, and `openrig-os-<version>-<board>.json`,
  which tells a pedalboard checking for updates which one it needs (see
  [docs/OS.md](docs/OS.md#updates)).

Versions follow [semantic versioning](https://semver.org) and come from
[conventional commits](https://www.conventionalcommits.org) (`feat:`,
`fix:`, `feat!:`…). [release-plz](https://release-plz.dev) keeps a release
PR up to date with the next version and `CHANGELOG.md`; only `feat:`, `fix:`
and `perf:` commits open one (`ci:`, `docs:`, `chore:`… wait for the next).
Nothing is released until it is merged: merging it tags the release, and
`.github/workflows/build-release.yml` attaches the binaries and images to it
(`os/release-assets.sh` makes the files of each board, the `.json` last).
That workflow also runs for a `vX.Y.Z` tag pushed by hand, and can be run by
hand to build a release again. The OS images take hours on GitHub's
runners: when a first build runs out of time, it saves its compiler cache
and running the failed job again resumes from it. Pull
requests run `cargo fmt`, `cargo clippy` and `cargo test`
(`.github/workflows/ci.yml`).

## Code layout

```
src/
├── main.rs              UI wiring (callbacks, polling and monitor timers)
├── bin/splash.rs        Boot logo on the Linux framebuffer (feature splash, OS image)
├── bin/bench.rs         pedalboard-bench: times a rig of NAM blocks on 1 to 8 cores
├── gpio.rs              Footswitches, encoder and I2C encoder polling (feature gpio)
├── hardware_ui.rs       Settings → Hardware (footswitches, encoders, their GPIOs), restarts the GPIO
├── strip_ui.rs          The rack's knob strip driven by external encoders
├── live_ui.rs           Live page: footswitch modes, tap tempo, hold → tuner
├── midi_io.rs           MIDI inputs (midir, feature midi): hot-plug, clock → tempo
├── midi_ui.rs           MIDI actions, learn in the block editor, Settings → MIDI
├── tone3000/            TONE3000: account (device flow, tokens), library (lists, search, downloads), limiter, pictures
├── tone3000_ui.rs       Settings → TONE3000 Connect, library page, Features.tone3000 flag
├── wifi.rs              Wi-Fi through NetworkManager (nmcli): scan, join, forget
├── wifi_ui.rs           Settings → Wi-Fi (worker thread running nmcli)
├── update.rs            Updates: latest GitHub release, app or system, download, request to the OS updater
├── update_ui.rs         Settings → Updates (worker thread: check, download, install)
├── store/               Plugin store: signed index, downloads, safe unpacking, receipts (feature store)
├── catalog_ui.rs        Store page (worker threads: index, pictures, install, removal)
├── monitor.rs           DSP meter, xruns, CPU / RAM / temperature / throttling
├── hardware.rs          What is wired (footswitch / encoder GPIOs, I2C boards): hardware.json, environment defaults
├── i2c_encoders.rs      I2C encoder boards (seesaw, M5Stack 8Encoder): protocol, events, a simulated bus
├── audio/
│   ├── jack_client.rs   JACK callback: tuner → effects → looper
│   ├── chain_switcher.rs  Builder thread, crossfade, live commands
│   ├── dsp_pool.rs      DSP worker threads: the chain on several cores
│   ├── engine.rs        Audio engine handle
│   ├── split_chain.rs   ChainNode / SplitType (chain description)
│   ├── tuner.rs, looper.rs
├── plugins/
│   ├── lv2_host.rs      lilv discovery (rescanned live) and instantiation
│   ├── lv2_features.rs  URID map/unmap, options
│   ├── lv2_worker.rs    LV2 Worker: slow plugin jobs on a thread of their own
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
third_party/i-slint-backend-linuxkms  Patched Slint KMS backend (see its PATCHED.md)
ui/                      Slint UI (app.slint, types.slint, pages/, components/, theme.slint)
os/                      Buildroot external tree: Pi 5 / x86_64 images, RAUC updates, init scripts
docs/                    Features, OS image, live (scenes, setlists, tempo), MIDI, routing, store, neural amp models (NAM)
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
- **JACK callback**: tuner → effects (with their returns and sends) →
  looper → output router, with preallocated buffers,
  `try_lock` only, and atomics for the DSP meter. A new chain is crossfaded in;
  the old one is dropped off the audio thread.

## Technical docs

- [docs/FEATURES.md](docs/FEATURES.md): every feature in technical detail
- [docs/OS.md](docs/OS.md): the OS image (Raspberry Pi 5, x86_64 mini PC): build, flash,
  settings, SSH, storage, boot logo, A/B updates
- [docs/SCENES.md](docs/SCENES.md): scenes, setlists, footswitch modes, tempo and tails
- [docs/MIDI.md](docs/MIDI.md): the MIDI map, learn and ranges, clock, MIDI to plugins
- [docs/ROUTING.md](docs/ROUTING.md): inputs, outputs, sends and returns, `routing.json`
- [docs/STORE.md](docs/STORE.md): the plugin store: charter, signed index, install and removal
- [docs/NEURAL_MODELS.md](docs/NEURAL_MODELS.md): the Amp / Amp + Cab / Pedal / Cabinet /
  Space blocks, TONE3000, roadmap
- [PROJECT_STATUS.md](PROJECT_STATUS.md): what works, what is left

## License

GPL-3.0-or-later (see [LICENSE](LICENSE)). The UI toolkit,
[Slint](https://slint.dev), is used under its GPL-3.0 license (its
royalty-free license does not cover embedded devices). Bundled third-party
code keeps its own license: NeuralAudio (MIT, with NAM Core and RTNeural),
the patched lilv bindings (MIT) and the patched Slint linuxkms backend
(GPL-3.0) in `third_party/`. The TONE3000 logos in `ui/assets/tone3000/`
are trademarks of TONE3000, not covered by the GPL, and only used for the
TONE3000 integration.

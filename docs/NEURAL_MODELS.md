# Neural amp models

The pedalboard has five built-in blocks that load a file, one per kind of
gear, so each one only offers the captures that make sense in it:

| Block | Engine | Plays TONE3000 gear | URI |
|---|---|---|---|
| **Amp** | NAM | `amp` (head only: add a Cabinet) | `urn:pedalboard:nam` |
| **Amp + Cab** | NAM | `amp-cab` (combo, or amp with its cab and mic) | `urn:pedalboard:nam-amp-cab` |
| **Pedal** | NAM | `pedal`, `outboard`, `experimental` | `urn:pedalboard:nam-pedal` |
| **Cabinet** | IR | `cab` | `urn:pedalboard:ir` |
| **Space** | IR | `space`, `experimental` | `urn:pedalboard:ir-space` |

The NAM blocks (feature `nam`, on by default) run
[Neural Amp Modeler](https://www.neuralampmodeler.com) captures natively,
without an LV2 plugin, so the chosen model is part of the chain and is saved
in presets. Amp and Cabinet keep the URIs of the former "NAM Amp" and "IR
Loader" blocks, so older presets still load.

## Why a built-in block

LV2 NAM plugins keep the model file in the LV2 state / patch extensions,
which this host does not implement: presets could not restore the model.
A native block also lets the UI list models, and later download them.

The block uses [NeuralAudio](https://github.com/mikeoliphant/NeuralAudio)
(MIT), the engine of the NAM LV2 plugin and of several hardware pedalboards.
It wraps NeuralAmpModelerCore and RTNeural, picks optimized implementations
for the common WaveNet / LSTM sizes, and is tuned for ARM, which matters on
a Raspberry Pi.

## Build

- `third_party/NeuralAudio` is a git submodule pinned to a commit:
  `git submodule update --init --recursive`.
- `native/nam_wrapper.cpp` is a small C interface: UTF-8 paths, loading
  errors returned as text, no C++ exception crossing into Rust.
- `build.rs` builds `native/` with CMake, **always in Release**: a debug
  build of the model is far too slow for real time.
- Needs `cmake` and a C++20 compiler (GCC 10+, Clang 12+). Build without the
  block with `--no-default-features --features winit,lv2,midi` (this also leaves
  out TONE3000; add `,tone3000` to keep it).

## Models

Models are read from `<data dir>/models` (`~/.local/share/pedalboard/models`
by default, see `PEDALBOARD_DATA_DIR`), subdirectories included:

```
models/
├── Plexi Crunch.nam
├── My Rig/
│   ├── clean.nam
│   └── crunch.nam
└── Tone3000/
    └── Amps/
        └── Peavey 5150 Blockletter [87953]/
            ├── tone.json
            ├── Peavey 5150 Blockletter Red [123456].nam
            └── Peavey 5150 Blockletter Red + TS9 [123457].nam
```

Impulse responses (`.wav`) for the IR blocks go in the same directory.

A **tone** is a folder of captures of the same gear (its **presets**); a
file at the top level is a tone with a single preset. The editor lists
tones, with their creator and number of presets: choosing one loads its
first preset, and a drop-down switches between the presets. Preset names
drop the words they all start with ("Boss MT-2 - Doom Metal" shows as
"Doom Metal"). A tone downloaded from TONE3000 has a `tone.json` (title,
creator, license, link, gear): a block only lists the tones of its gears;
tones without `tone.json` are listed in every block of the right format.

A list of more than 8 tones or presets (impulse responses too) has a
**Search** field at the top: it opens the on-screen keyboard and keeps the
entries whose title or subtitle (creator, number of presets) holds every
word typed, e.g. `5150 red`.

Supported model files: `.nam` (WaveNet, LSTM, A2), RTNeural keras `.json` and
AIDA-X `.aidax`. Free captures: [Tone3000](https://www.tone3000.com).

Presets store the path relative to the models directory, so presets and
models can be copied to another pedalboard together.

## Amp and Pedal blocks

| Control | Range | Purpose |
|---|---|---|
| Input | -24…24 dB | Drive into the model |
| Output | -24…24 dB | Level after the model |
| Normalize | on/off | Apply the input / output levels recommended by the model metadata, so captures have similar loudness |
| Gate | on/off (off) | Noise gate |
| Threshold | -100…0 dB (-80) | Level under which the gate closes |
| Bass, Middle, Treble | 0…10 (5 = flat) | Tone stack, **Amp and Amp + Cab only** |
| Quality | Full / Light (Full) | Size of the model used, **only for files holding several sizes** |

The gate and the tone stack work like the ones of the NAM plugin, so the
same settings sound the same:

- The gate measures the **clean input** of the block and turns down the
  **model output**: it follows the playing, not the hiss the capture adds.
  Below the threshold the gain drops by 0.1 × (dB below)², opens in 5 ms,
  holds 10 ms and closes in 50 ms. It is off by default: one gate, on the
  first high-gain block, is usually enough.
- The tone stack is three filters after the model: low shelf at 150 Hz
  (±20 dB), peak at 425 Hz (±15 dB, narrower when cutting) and high shelf
  at 1.8 kHz (±10 dB). A centred knob costs nothing. Pedals have no tone
  stack: their capture already includes their tone knob.

Order in the block: input level, model, gate, tone stack, output level.

### Quality

TONE3000 serves A2 captures as packed files (`SlimmableContainer`): one
`.nam` holding the capture in two sizes, a full one and a light one. Both
are loaded, so Quality switches between them while playing, without a
dropout. Light costs about a quarter of Full (a full A2 capture is about
12 % of a Raspberry Pi 5 core at 128 frames), for a slightly less accurate
tone: worth it for a booster or a pedal before the amp, or to fit more
blocks. The control only shows for such files; a model trained at one size
(A1 standard, lite, feather, nano) cannot be made smaller.

The editor header shows the share of a CPU core the block uses, measured
while it plays (0 when bypassed).

Without a model, a block passes audio through (with its gains). A model
that fails to load is reported in the UI and the block is skipped.

Models are trained at 48 kHz: run JACK at 48 kHz (96 kHz works for WaveNet
models through oversampling). At 44.1 kHz the tone is off and a warning is
logged.

Loading happens on the chain builder thread, never in the audio callback.

## CPU on the Raspberry Pi

A standard WaveNet capture is the most expensive block of a chain. Watch
the DSP load in the resource monitor; prefer "lite" or "feather" captures
when stacking several models (see Quality above for measured figures).

## Cabinet and Space blocks

These blocks convolve the signal with an impulse response (`.wav`, any
sample rate, 16/24/32-bit or float). In stereo mode, stereo files are
convolved per side (a stereo room makes the signal stereo), with the same
normalize gain on both sides; in mono mode their channels are averaged:

- **Cabinet**, after an amp-only capture (Mix 100 %);
- **Space**, rooms and reverbs (Mix 35 % by default).

| Control | Range | Purpose |
|---|---|---|
| Level | -24…24 dB | Output level of the convolved signal |
| Mix | 0…100 % | Dry / wet balance (100 % for a cabinet) |
| Normalize | on/off | Scale the response to unit gain over the guitar band (80 Hz–5 kHz), so IRs have similar loudness |
| Low cut | 20…500 Hz (20 = Off) | High-pass on the convolved signal (boomy cab, muddy reverb) |
| High cut | 1…20 kHz (20 kHz = Off) | Low-pass on the convolved signal (fizz, bright rooms) |

The cuts are 12 dB/octave Butterworth filters on the wet signal only: on a
Space block, the dry signal keeps its full range.

On load the file is resampled to the audio rate, trailing silence below
-80 dB is cut, and responses are limited to 10 s.

The convolution has no latency and a flat cost per audio block: the first
170 ms of the response (any cabinet) are convolved in the callback; the
rest of a room response is convolved on a worker thread, which has 85 ms to
deliver each part. If the CPU is so overloaded that the worker is late, a
piece of the reverb tail is dropped rather than blocking the audio.

## Tone3000 gear types

Tone3000 sorts captures by **gear** and **format**. What each means here:

| Gear | What it is | Block |
|---|---|---|
| `amp` | Amp head only, no speaker | Amp, followed by a Cabinet |
| `amp-cab` (formerly "full rig") | Combo, or amp with its cabinet and mic | Amp + Cab, no Cabinet after it |
| `pedal` | Overdrive, fuzz, boost… | Pedal, before the amp |
| `outboard` | Studio gear: preamp, compressor, EQ, tape… | Pedal |
| `cab` | Cabinet (almost always an IR) | Cabinet |
| `space` | Rooms, halls, reverbs (IRs) | Space |
| `experimental` | Anything else | Pedal or Space depending on the format |

| Format | Supported |
|---|---|
| `nam` | Yes (Amp, Amp + Cab, Pedal), A1 and A2 architectures |
| `ir` | Yes (Cabinet / Space) |
| `aida-x` | Local files only (neural blocks, `.aidax` / `.json`); not downloadable from the library yet |
| `proteus` | Probably (RTNeural keras `.json` through the neural blocks); not tested |
| `aa-snapshot` | No |

Model sizes (`standard`, `lite`, `feather`, `nano`): smaller models cost
less CPU and sound slightly less accurate. On a Raspberry Pi, prefer `lite`
or `feather` when several NAM blocks are stacked.

## Roadmap

1. ✅ NAM block, model chooser, models saved in presets.
2. ✅ IR blocks (cabinets and spaces).
3. ✅ TONE3000 Connect (Settings): the user enters the publishable key of
   their own TONE3000 application (`t3k_pub_…`; a secret key `t3k_cs_…` is
   refused), the pedalboard shows a QR code and a short code, approved on a
   phone at tone3000.com/activate. Tokens are kept in
   `<data dir>/tone3000.json` (owner-only), refreshed when they expire (the
   refresh token rotates); a revoked link signs out, being offline does not.
   Once connected, `Features.tone3000` turns the TONE3000 features on.
4. ✅ TONE3000 library: favorites, own tones, downloaded tones, trending and
   latest, filtered by gear (the API ignores the gear filter on "latest",
   so it is applied locally). Opening a tone lists its models (A1 and A2:
   `GET /models` only returns A2 models with `architecture=2`, so both are
   asked). Each model is downloaded only on request, one at a time (the free API
   tier allows no bulk download), into
   `models/Tone3000/<gear>/<title> [<id>]/` (as `<model> [<model id>].nam`,
   the ids hidden in the UI), with a `tone.json` keeping the
   creator, license and link; the token is only ever sent to tone3000.com.
   Opened from a block, the library only offers that block's gears and
   "Download" fetches a model, then "Use" loads it in that block; otherwise (menu ⋮, or a tone of another
   gear) "Add" puts the model in a new block of the right type at the end
   of the chain. NAM and IR
   formats only for now (AIDA-X, Proteus and AA snapshots are listed but
   greyed out). Favoriting from the pedalboard and catalogue search are not
   part of the free API tier: favorite on tone3000.com, and ask TONE3000
   about search (or the `select_tone` flow) once the project is public.
5. ✅ One block per gear (Amp, Amp + Cab, Pedal, Cabinet, Space), tones listed as
   folders with a preset drop-down.
6. ✅ Noise gate on the neural blocks, tone stack on the amp blocks, low /
   high cut on the IR blocks.
7. Search: `select_tone` (TONE3000-hosted search, needs a browser and a
   redirect URI) or `/tones/search` if TONE3000 grants it.

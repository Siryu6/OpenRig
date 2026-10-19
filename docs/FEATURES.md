# Features

What OpenRig does, in more technical detail than the
[user guide](https://openrig.eu/docs/).

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
  (Light is about four times cheaper). Saved in presets. See [NEURAL_MODELS.md](NEURAL_MODELS.md).
- **Cabinet and Space blocks**: impulse response loaders (`.wav`) for
  cabinets and rooms, with level, dry/wet mix (35 % by default on Space),
  loudness normalization (unity gain over the guitar band, 80 Hz – 5 kHz) and low / high cut. Zero latency; the tail of long room responses is
  convolved on a worker thread so the audio callback cost stays flat.
- **Wi-Fi** in Settings: scan, join (on-screen keyboard with symbols, hidden
  password), disconnect, forget, radio on/off. It drives NetworkManager
  through `nmcli` (as on Raspberry Pi OS); elsewhere (macOS, Windows, Linux
  without NetworkManager) Settings says Wi-Fi is managed by the system.
- **Light theme**: Settings → Display switches the screen to light panels
  and dark text, for daylight; dark stays the default. The rack blocks,
  knobs, faders, footswitches and LEDs keep their dark hardware look. Kept
  in the session (`light_theme`).
- **Power off / restart** from the menu ⋮ on the OS image (the launcher sets
  `PEDALBOARD_POWER_REQUEST`; the app saves the session and quits, the
  launcher switches the board off). Hidden on a desktop.
- **TONE3000 Connect** (feature `tone3000`, default): Settings → TONE3000
  Connect links a [tone3000.com](https://www.tone3000.com) account with the
  OAuth device flow (QR code and short code on the screen, approved on a
  phone). Each user enters the *publishable* key (`t3k_pub_…`) of their own
  TONE3000 application; no key ships with the code. Once connected, the
  TONE3000 features are enabled (`Features.tone3000` flag in the UI): the
  **TONE3000 library** (menu ⋮, or the T3K Browse button of a block)
  lists your favorites, your own and downloaded tones, and the trending /
  latest ones, by gear, each with its picture, format and creator (avatar
  and username), following the TONE3000 design requirements. Pictures are
  fetched when shown and kept in memory only. Models are downloaded one at a time
  when you ask, into `models/Tone3000/<gear>/<tone>/` with the creator,
  license and link in `tone.json`, and can be loaded straight into the block.
  Opened from a block, the library only shows the gears that block plays;
  from the menu, "Add" puts a downloaded model in a new block of its type.
  A block playing a TONE3000 tone carries the T3K mark, and its editor
  shows the tone picture.
- **Chain with splits**: serial chain plus nested A/B, Mix and L/R splits;
  a split's type can be changed from the rack (its blocks are kept).
- **Inputs and outputs**: every input and output of the interface (up to
  16 each). One input feeds the effects; each output or stereo pair plays
  the main mix, one of 4 sends, or an input passed straight through, with
  level, balance, mute, solo and polarity (kept for the rig in
  `routing.json`). Send and Return blocks, placed anywhere in the rack,
  copy the signal at their place to a send bus or bring an input in, with
  level, mix and bypass per scene. See [ROUTING.md](ROUTING.md).
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
  parameter values by port symbol, so plugin updates keep them, block
  colours) with 8 scenes and a tempo. A scene
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
  See [MIDI.md](MIDI.md).
- **Hardware** (feature `gpio`): footswitches follow the Live page mode
  (act when pressed; the tap switch also reads the hold), and keys `1`–`8`
  and `t` do the same on a keyboard; the rotary encoder drives the UI like
  the arrow and Return keys. Settings → Hardware sets what is wired: from
  no footswitch to 8, the GPIO of each (with a LED that lights while it is
  pressed, to check the wiring), the encoder or none, its push button or
  none, its steps per detent. Kept in `hardware.json` in the data
  directory; changes apply at once.
- **OS image** (`os/`): Buildroot image for the Pi 5 and x86_64 mini PCs,
  `PREEMPT_RT` kernel, read-only system, boot logo, the UI full screen
  without X11 / Wayland, signed A/B updates (RAUC) that roll back when the
  new system does not start. See [OS.md](OS.md).
- **Updates** (feature `update`, default): Settings → Updates checks the
  latest GitHub release and, on the OS image, installs it: the app alone
  when the system did not change, else the whole system. See
  [Updates](https://openrig.eu/docs/setup/updates/).

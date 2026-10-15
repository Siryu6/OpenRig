# Project status

Last update: 2026-10-15.

## Working

- LV2 discovery and hosting through lilv, tested end to end with a compiled
  test plugin (`tests/lv2_hosting_test.rs`).
- Chain with nested A/B / Mix / L/R splits, processed in the JACK callback
  without allocation; crossfaded chain switches; live parameter, bypass, A/B
  and mix changes.
- Rack view: nested splits are laid out without overlap, every block is
  connected, and the canvas scrolls in both directions.
- UI and audio share one chain state (`AppState`): every edit in the UI is
  heard.
- Parameter editor generated from the plugin ports (float, int, toggle, enum).
- Presets with the full chain and parameter values (older flat presets still
  load), setlists with preloaded presets, session restored at startup.
- Scenes in a preset (8, Helix snapshot style): bypass, parameters and split
  settings recalled as live commands, without rebuilding the chain; editing
  the current scene, copy into an empty one, rename. Unit tested, and checked
  in the UI under Xvfb.
- Tails: delay / reverb LV2 blocks and the Space block ring on when bypassed
  (input faded, output kept until 0.5 s of silence). Unit tested.
- Live page: Stomp (assignable blocks) / Scenes / Setlist modes on 8 tiles,
  tempo per preset with tap tempo and − / +, sent to LV2 BPM ports
  (`time:beatsPerMinute` designation or BPM unit). The last footswitch taps
  the tempo; held 0.8 s it opens the tuner, any switch closes it. Keys
  `1`–`8` and `t` on a keyboard.
- Tuner, looper (overdub layers, undo), GPIO footswitches (press and
  release edges) and encoder.
- Resource monitor: DSP load of the audio callback, JACK load, xruns, CPU,
  memory, temperature, Raspberry Pi under-voltage / throttling. Chain
  crossfades (e.g. mono / stereo switch) are left out of the DSP load. The
  block editor shows the share of a CPU core the block uses.
- Amp, Amp + Cab and Pedal blocks: `.nam` captures and RTNeural models through
  NeuralAudio, chosen in the editor by tone (folder) then preset (drop-down),
  filtered by gear, saved in presets and the session. Noise gate (NAM
  style: detected on the input, applied after the model) and, on the amp
  blocks, a bass / middle / treble tone stack. Tested with the NAM Core example models (WaveNet, LSTM).
  Full / Light quality on packed A2 captures (both sizes loaded, switched
  without a dropout), unit tested (`src/plugins/nam.rs`).
- Cabinet and Space blocks: `.wav` cabinets and rooms (any rate, resampled; stereo files
  convolved per side in stereo mode), level / mix / normalize, low / high cut. Constant callback cost (≈3 % of a
  128-frame period on a desktop CPU, even for an 8 s room).
- Wi-Fi in Settings through NetworkManager (`nmcli`): scan, join with a
  password (keyboard with symbols, hidden text), disconnect, forget, radio
  on/off; "managed by the system" without NetworkManager. Scan and status
  tested on a desktop; joining not tested yet on the Pi.
- TONE3000 Connect in Settings: device flow with the user's own publishable
  key, QR code, token refresh, `Features.tone3000` flag. Tested against the
  live API (sign-in, refresh, `/user`).
- TONE3000 library (shown when connected): favorites / own / downloaded /
  trending / latest by gear, models of a tone (A1 + A2), download on
  request into `models/Tone3000/`, "Use" from a block (the library then shows that block's gears only). Tested
  against the live API (list, models, download).
- Hardware-style UI: near-square corners, amber accent, muted block colours,
  input / output peak meters (measured in the JACK callback, 2 dB LED
  segments, latched clip LEDs), key-like buttons with LEDs, recessed displays, knobs
  tinted with the block colour, LED tuner ramp, footswitch tiles with LEDs.
- Rack: gear-shaped blocks, colours by family or per block (saved in presets and the
  session), tap to select with quick knobs at the bottom (paged), double tap
  for the editor, hold to bypass, hold and drag to move blocks (into split
  branches too). The encoder does not turn the quick knobs yet. Cuts show "Off" at their off end. Tested with injected touch events
  under Xvfb.
- Stereo: two JACK outputs, L/R splits to separate sides, pan per branch
  on Mix splits, mono / stereo lines drawn in the rack, dual-mono IR and
  LV2 blocks after the stereo point, stereo looper and preset crossfade,
  split type change from the rack. Tested with unit tests (and a real LV2
  plugin), not yet on a stereo interface.
- MIDI (see `docs/MIDI.md`): all inputs heard and hot-plugged (250 ms scan),
  Helix-style map (PC / CC0 banks, setlist slots and CC32 setlists, scenes,
  footswitches, looper, tap, tuner, modes, preset step), clock → tempo,
  channel / omni, MIDI learn in the block editor (parameters and on / off),
  saved with the preset, with a heel / toe range per control (reversible);
  the learned controller plays live while learning to try the range. Tested
  under Xvfb with a virtual ALSA source (clock, PC, CC, learn, range,
  control); not yet with a real interface.
- LV2 atom inputs: the tempo goes to plugins taking `time:Position` (as a
  rolling transport), and MIDI notes, pitch bend, pressure and non-global
  CCs to plugins taking `midi:MidiEvent`, on their own queue (never in the
  command replay log). Tested with a real LV2 plugin compiled by the test
  (`tests/lv2_events_test.rs`).
- OS image (`os/`, see `docs/OS.md`): Buildroot external tree for the
  Raspberry Pi 5 and x86_64 mini PCs, `PREEMPT_RT` kernel, BusyBox init,
  read-only root with a data partition created on first boot, settings in
  `pedalboard.txt` on the FAT partition, jackd and the UI as restarted
  services, SSH by key, a boot logo on the framebuffer until the UI takes
  over. The UI runs full screen without X11 / Wayland
  (feature `kms`, Slint linuxkms) and restarts with the audio when the JACK
  server stops or starts (`PEDALBOARD_EXIT_ON_AUDIO_CHANGE`). Both images
  build with Buildroot 2026.02.3 (about 600 MB each). The x86_64 one is
  tested in QEMU (`make -C os qemu-x86_64`): first boot creates `/data`,
  jackd starts on the emulated sound card, the UI shows full screen about
  4 s after the kernel starts, restarts with the audio when jackd is killed,
  and `/data` is kept across reboots.
- OS updates without reflashing: two system slots (A/B) and RAUC bundles
  (`update.raucb`, signed with the build's key in `os/keys`), installed
  over SSH with `pedalboard-update install` / `reboot`. An update is tried
  once and kept only if the UI runs for 30 s; otherwise the board comes back
  on the previous system. Pi 5: one FAT boot partition per slot and
  `autoboot.txt` tryboot; x86_64: GRUB `grubenv`.
- 121 tests (`cargo test`), no clippy warning on the default, `lv2`,
  `lv2,nam`, `gpio`, `kms`, image (`kms,lv2,nam,tone3000,midi,gpio,splash`) and
  no-feature builds. `rust-version` 1.88 (the image's rustc).

## Not tested yet

- The x86_64 image on hardware (only in QEMU): linuxkms on Intel graphics,
  boot time on real storage.
- On the Raspberry Pi: the Pi 5 image runs with a Scarlett 2i2 at 128 x 2
  (0 xrun), HDMI, Wi-Fi, TONE3000 and A/B updates. A NAM block (A2 capture)
  costs about 12 % of one core, an IR block almost nothing. Still to check:
  latency, GPIO wiring (switch hold and debounce), touchscreen, the firmware
  throttling readout (`get_throttled`).
- MIDI with real hardware (AudioBox DIN input, USB controllers), on macOS
  (CoreMIDI) and Windows (WinMM).
- Keys `1`–`8` / `t`: not reachable under Xvfb without a window manager; the
  tiles and the on-screen TAP key were tested instead.
- Third-party LV2 plugins taking MIDI or the transport (synths, x42 / MOD
  tempo-synced effects): the atom path is tested with a test plugin only.
- LV2 state and file loading through atoms (patch:Set) are not handled.

## Limits and ideas

- Rack drag and drop: no auto-scroll when dragging near the screen edge;
  long block names are limited to about two lines.

- MIDI: input only (no MIDI out, no PC sent to other gear), CC62 and
  CC65–67 of the Helix map and Start / Stop are ignored. Expression pedals
  come over MIDI (CC learn); an analog expression input is not handled.
  All MIDI plugins get the same messages (no per-block MIDI routing).
- The transport sent to LV2 plugins is always rolling at bar 1, beat 1: no
  bar position, so plugins that align to the bar (sequencers) do not.
- A preset change or a structural edit crossfades to a new chain: tails
  stop there (spillover across presets would need both chains running).
- Footswitches are fixed at 8 slots in the UI; more switches would need
  pages (banks).

- Mono input only: a second input (stereo keys, a stereo send) is not
  handled yet.
- NAM blocks after the stereo point play the mix of both sides (a second
  model per side would double their CPU cost).
- No LV2 state extension: LV2 plugins whose settings live in state (model
  files, IR loaders) do not restore them from presets. The built-in NAM block
  does not have this problem.
- TONE3000 search is not in the free API tier: the library lists
  favorites, own, downloaded, trending and latest tones; contact TONE3000
  once the project is public (see `docs/NEURAL_MODELS.md`).
- AIDA-X / Proteus TONE3000 formats are not downloadable yet.
- Changing the model (or any structural edit) reloads every model of the
  chain on the builder thread; a model cache would make edits faster with
  several NAM blocks.
- No worker extension: plugins requiring `work:schedule` are skipped.
- Moving a Mix split's pan across the centre rebuilds the chain in stereo
  mode (the split switches between mono and stereo output): delay and
  reverb tails restart. Bypassing a split is instant (no ramp).
- The looper shares a mutex with the UI: when the UI holds it, the audio
  callback skips the loop for one block. An RT-owned looper driven by a
  command queue would remove this.
- OS: updates are installed over SSH only (no download or USB install from
  the UI yet, no release publishing); no watchdog, so a system that hangs without a panic needs a
  power cycle to go back to the previous slot. No LV2
  plugins in the image, no AMD graphics on x86_64, no Wi-Fi hotspot for the
  first set-up. The screen used for the POC lights up after the UI starts,
  so the boot logo only shows at power off.

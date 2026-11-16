# Features

What OpenRig does, in more technical detail than the
[user guide](https://docs.openrig.eu/).

- **LV2 hosting**: plugins are discovered with lilv (`LV2_PATH` or the
  standard locations). Controls, ranges, units, toggles, integers and
  enumerations are read from the plugin metadata and edited in the UI. URID
  map / unmap, options, bounded-block-length, isLive and the worker
  (`work:schedule`: slow jobs such as loading a model run on a thread of the
  plugin's own) are provided; plugins that need anything else are skipped,
  with a warning naming the missing feature in the log. Atom inputs get the
  transport (`time:Position`: a running bar and beat position at the tempo,
  in 4/4, at every block) and MIDI. Without any installed plugin, a demo
  catalogue (passthrough) keeps the UI usable for development.
- **Plugin store** (feature `store`): installs free LV2 plugins from a
  signed index (minisign, anti-rollback serial, expiry), with the archive
  checked by size and sha256 and unpacked safely into `<data>/lv2`; the
  plugins are scanned again without restarting. Bundles copied by hand into
  that folder are always loaded and never touched by the store. The
  picture of a catalog plugin also shows on the Add effect page, and stands
  for the block in the rack when it is the gear's face. See
  [STORE.md](STORE.md).
- **Plugin views**: above an LV2 block's controls, the editor draws an EQ's
  response, a compressor's level curve or a delay's echoes (each with its
  fading tail, over the beats of the tempo), with points to drag, over the sound measured in and out of that block (spectrum, levels).
  A drive shows its tone: its small-signal frequency response, measured
  on an instance of its own at the current settings (a quiet click after a
  short silence, on the chain builder thread) and drawn again when a
  control moves. They are inferred from the plugin's LV2 class and the names and
  units of its ports, never configured per plugin; a plugin read without
  confidence keeps its knobs alone.
- **OpenRig FX faceplates**: plugins whose URI starts with
  `https://openrig.eu/plugins/` (the [OpenRig FX](https://github.com/Siryu6/openrig-fx)
  suite) may describe their face in Turtle, in the `https://openrig.eu/ns/view#`
  namespace: `openrig:family` (`Basic` or `Signature`), `faceColor`
  (`"#RRGGBB"`), `knobStyle` (`Plain`, `Skirted`, `Chickenhead`, `Domed`),
  `finish` (`Painted`, `Brushed`), `wear` (0 to 1), `plateName` (12
  characters at most), `plateStyle` (`Silkscreen`, the default, or
  `Glitch`), `tagline` (48 at most), `view` (`EqView`,
  `DynamicsView` with `dynamics` `Compressor`, `Limiter` or `Gate`,
  `EchoTimelineView`, `StepGridView`; a view the app does not draw leaves
  it to inference)
  and, per control, `order` (the face order: controls the user has not
  arranged follow it, then the port order), `role` (what the control means
  to the view: `threshold`, `ratio`, `knee`, `makeup`, `inputGain`, `range`,
  `gainReduction`, `delayTime`, `feedback`, `mix`, `wetLevel`,
  `delayTempoDivision`, `pingPong`) and `primary` (its rank in the rack's
  knob strip, before the other controls, unless the user arranged them).
  A stated view or role wins over what the view infers from names; the
  rest is inferred as for any plugin. With ping-pong on, a delay's echoes
  alternate between a left lane (top) and a right lane (bottom); a toggle
  named "Ping Pong" is read the same way on any delay. A `StepGridView`
  (a stutter, slicer or rhythmic gate) is drawn only from the plugin's
  `openrig:stream` atom output of content `StepGrid` (3 rows: level,
  source, speed), never from its controls: one bar with a line on each
  beat, the live input in grey, the slice captured outlined in amber and
  its repeats filled, the speed as a line where it is not 1, an arrow
  where it plays backward, and the playhead. The audio thread copies the
  last frame of the selected block only while its editor is open (a
  seqlock in `BlockTap`, no allocation); the UI reads it every 40 ms,
  draws the cells again only when they change, merges cells narrower than
  4 px, and moves the playhead by whole pixels. Any other plugin, or one with no family, keeps the generic
  look; these terms are ignored outside that prefix, and a signature plugin
  without a valid colour is drawn as a basic one. A basic plugin has the
  OpenRig look: an amber left edge and an "OR FX" mark on its rack block with
  a glyph of its category, the OpenRig mark in the editor header (and
  "OpenRig FX" before the family, latency, parameters and CPU; on a screen
  under 1000 px wide, only the family, latency and CPU), its view always
  shown, and modes of three choices or fewer always as a switch: keys in a
  row, stacked when the control is too narrow for them (on a signature
  plate, a column of silk labels each with an LED). Choices come in the
  order of their values on every plugin.
  A signature plugin's controls and view sit on its plate in the editor:
  a picture drawn once per plate and size by `Plate::render` (its colour lit
  from above, grain, brushed streaks, scratches and rubbed edges from the
  wear, seeded by the URI so a plate never changes) and kept in memory, four
  screws, knobs in its knob style (a steel cap on a very dark plate), values
  in amber readout windows, a view window that stays dark with amber
  lines in both themes (the gear colours of `Theme.plot-gear`), its silk name, tagline and green "on" LED. A
  `Glitch` plate prints its name cut by two thin bands, as a corrupted
  picture: from 20 to 30 % of the cap height above the baseline moved
  right by 8 % of it in amber, from 62 to 70 % moved left by 4 %. The
  offsets follow the font's own cap height and fall on whole pixels, so
  the editor's plate and the rack's small pedal (whose name drops to
  10 px when it does not fit) show the same cut; it never moves. The
  web remote draws plates flat, in their colour only. A plate without a view
  gives its height to its controls when they fit in two rows: taller cells
  and knobs up to 104 px, the rows centred on the plate.
  The parameter grid shows whole rows only: as many as fit at 110 px or
  more share its height, the others go to pages (up and down keys and a
  page count, silk-screened on a plate). On tall screens a view leaves room
  for two rows. A plate's page keys stay dark in both themes.
  In the rack, a signature block is a mini pedal on its plate: two small
  knobs, the green LED, its silk name and a footswitch (tap, hold and drag
  as on any block; bypassed, it goes dark). Its default colour, on the rack,
  the knob strip and in an exported rig (`color_hex`; `color` stays the
  family's palette name), is its plate colour, lifted when too dark to show
  on the stage.
  The Add effect page and the store mark the suite's plugins with an
  "OR FX" badge (by URI prefix, so before they are installed too) and show
  an installed one's block as the rack draws it: the same `BlockShape` and
  `BlockFace`, with the plate drawn at twice the rack size on a store page.
- **Amp, Amp + Cab and Pedal blocks** (feature `nam`, default): built-in neural blocks
  running Neural Amp Modeler `.nam` captures (and RTNeural `.json` /
  `.aidax` models) through [NeuralAudio](https://github.com/mikeoliphant/NeuralAudio).
  The editor lists tones (a folder of captures, e.g. one TONE3000 tone)
  rather than files: choosing a tone loads its first preset, and a drop-down
  switches between its presets. Long lists have a search field. Each block lists the tones of its
  gear first (Amp: heads; Amp + Cab: combos and amps with their cab;
  Pedal: pedals, outboard, experimental), then every other tone it can load. Each has a noise gate (as in the NAM plugin: it listens to
  the clean input and gates the model output); the amp blocks add a bass /
  middle / treble tone stack (after the model, or before it with EQ
  position Pre), and packed A2 captures a Full / Light quality
  (Light is about four times cheaper). Saved in presets. See [NEURAL_MODELS.md](NEURAL_MODELS.md).
- **Cabinet and Space blocks**: impulse response loaders (`.wav`) for
  cabinets and rooms, with level, dry/wet mix (35 % by default on Space),
  loudness normalization (unity gain over the guitar band, 80 Hz – 5 kHz) and low / high cut. Zero latency; the tail of long room responses is
  convolved on a worker thread so the audio callback cost stays flat.
- **Wi-Fi** in Settings: scan, join (on-screen keyboard with symbols, hidden
  password), disconnect, forget, radio on/off. It drives NetworkManager
  through `nmcli` (as on Raspberry Pi OS); elsewhere (macOS, Windows, Linux
  without NetworkManager) Settings says Wi-Fi is managed by the system.
  The card also shows the IPv4 address of each connected Ethernet and Wi-Fi
  device, to reach the pedal over SSH.
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
  phone), each user with their own account. The pedalboard signs in as the
  OpenRig application (its *publishable* key `t3k_pub_…` is built in); a
  developer can enter the key of their own application instead. Once connected, the
  TONE3000 features are enabled (`Features.tone3000` flag in the UI): the
  **TONE3000 library** (menu ⋮, or the T3K Browse button of a block)
  lists your favorites, your own and downloaded tones, and the trending /
  latest ones, and searches TONE3000 (sent when the search is submitted), by gear, each with its picture, format and creator (avatar
  and username), following the TONE3000 design requirements. Pictures are
  fetched when shown and kept in memory only. A tone's models are shown
  25 per page, with "Select all" for the page shown (25 models ticked at
  most, as TONE3000 asks). The models you tick are
  downloaded one after the other, at a pace that stays under the TONE3000
  rate limit (as every request), into `models/Tone3000/<gear>/<tone>/` with the creator,
  license and link in `tone.json`, and can be loaded straight into the block.
  Opened from a block, the library lists the tones of the block's format
  ("All NAM" or "All IRs") and offers every gear filter; it never opens on
  a gear the user did not pick. "Use" loads a downloaded model in it (any
  IR in Cabinet or Space, any NAM model in Amp, Amp + Cab or Pedal);
  "Add" puts a downloaded model in a new block of its type.
  A block playing a TONE3000 tone carries the T3K mark, and its editor
  shows the tone picture.
- **Chain with splits**: serial chain plus nested A/B, Mix and L/R splits;
  a split's type can be changed from the rack (its blocks are kept).
- **Latency compensation**: a plugin that reports latency
  (`lv2:reportsLatency`, e.g. a lookahead compressor or a pitch shifter)
  stays in time with the rest. The other branch of a split, the dry signal
  of its bypass and a Return after it are delayed to match, so a wet/dry mix
  has no comb filtering (up to 100 ms per path). A change of latency fades
  over 10 ms, without a click. The total is reported to JACK, so a recorder
  can align what it records. See [ROUTING.md](ROUTING.md#latency).
- **Inputs and outputs** (⋮ → Routing): every input and output of the
  interface (up to 16 each). One input feeds the effects; each output or stereo pair plays
  the main mix, one of 4 sends, or an input passed straight through, with
  level, balance, mute, solo and polarity (kept for the rig in
  `routing.json`). Each output or pair also has a 6-band EQ (low and high
  cut, low and high shelf, two mids, drawn on a curve you drag) and a
  limiter that keeps peaks under a ceiling (on by default, −1 dBFS, with
  a gain reduction meter). Send and Return blocks, placed anywhere in the rack,
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
  a plain card, OpenRig FX plugins their faceplate), coloured by family (amp red, cab orange, drive yellow,
  reverb teal, delay green, modulation blue, pitch lime…) or with a colour picked in the
  block editor. The UI is styled like dark, minimal hardware: near-square
  corners, amber accent, key-like buttons with status LEDs, recessed displays,
  input / output LED meters with clip LEDs on the rack, knobs tinted with the block's colour, LED
  tuner ramp, footswitch tiles on the Live page. Tap to select:
  the block's controls show at the bottom of the rack (five per page)
  for quick tweaks: knobs, switches, and lists that open just above the
  strip, then the plugin's factory presets and its model (the NAM capture,
  IR, sample…: the presets of the same file type to switch to, or the
  file picker); double tap opens the full editor, whose parameter grid
  goes by pages (arrows on the right) when it does not fit the screen. Arrange, in the
  editor, sets the order of a plugin's controls and which ones the knob
  strip shows, for every instance of the plugin (Reset goes back to the
  plugin's order). Hold (0.35 s) and
  release in place to bypass, hold and drag to move a block anywhere,
  including into a split branch. A quick swipe scrolls the rack.
- **Real-time safe audio**: chains are built off the audio thread and swapped
  with a 20 ms equal-power crossfade. Parameters, bypass (10 ms ramp), A/B and
  mix changes are applied live without rebuilding. The JACK callback does no
  allocation and takes no blocking lock.
- **Plugin guard**: a plugin that takes more than half of the audio period
  for about 0.3 s, three quarters of it a few times in quick succession, or
  more than the whole period five times within a few seconds, is switched
  off (bypassed) so it cannot freeze the board. So is a plugin
  that keeps putting out invalid samples (NaN); a single bad block is
  silenced. A toast says why ("Delay was switched off: it used too much
  CPU"); switch the block back on to try again.
- **Multi-core DSP**: a heavy rig runs on up to 8 cores (as many as the CPU has: one on a single core) with no added
  latency and the same samples as on one core. The branches of a split play
  at once on two cores; a long chain is cut into stages of about the same
  cost, each on its core, that pass every period along in 8 slices (a stage
  starts a slice as soon as the one before has finished it). During a preset
  change, the old and new chains play on two cores. Light rigs stay on one
  core; a block feeding a send bus stays on the audio thread. Settings →
  Processing sets the cores (Auto: all of them). `pedalboard-bench` times a
  rig of NAM blocks offline on 1 to 8 cores.
- **Presets, scenes and setlists**: presets store the whole chain (splits,
  parameter values by port symbol, so plugin updates keep them, block
  colours) with 8 scenes and a tempo. A scene
  recalls bypass, parameters and split settings instantly, without
  rebuilding the chain; a setlist maps footswitch slots to presets, all
  preloaded in memory.
- **Rigs**: share a preset as a `.openrig` file with the plugins and models
  it needs (built-in, store, TONE3000 or local). Importing one installs
  what the store and your TONE3000 account can provide, then adds it as a
  new preset. Files live in `<data>/rigs` and on USB drives (the OpenRig
  OS mounts them when plugged in; **Eject** before pulling them out). The
  **openrig.eu** tab browses, installs, favourites and publishes rigs on
  the community site, with the pedalboard linked to an account by a code.
  See [RIGS.md](RIGS.md).
- **Live page**: 8 switch tiles in Stomp (block on/off, assignable),
  Scenes or Setlist mode, tempo display with TAP. A stomp switch can also
  play a switch of its plugin (Hold, Freeze…), latching or momentary (on
  while held, from a footswitch, a tile, a key or MIDI CC 49–56). The last footswitch taps
  the tempo and, held, opens the tuner. The tempo drives the BPM ports of
  LV2 plugins and the bar position of the plugins that follow the
  transport: a tap lands on a beat, a MIDI Start on the first beat.
- **Tails (spillover)**: delays and reverbs keep ringing when bypassed or
  when the scene changes. LV2 plugins with an `lv2:enabled` port keep
  running bypassed with that port at 0, and play their own tail.
- **Session**: the current chain is saved a few seconds after each change (and on exit)
  and restored at startup, so switching the pedalboard off is safe.
- **Backup and restore**: Settings → Backup writes presets, setlists, rigs,
  loops and settings to one `.tar.gz` file on a USB drive (or on the pedal),
  with models and plugins as an option. Without them, store plugins and
  TONE3000 models are downloaded again after a restore. A restore is
  checked first, applied at the next start with one undo, and never touches
  passwords, accounts or Wi-Fi. The remote control downloads and uploads
  backups too. See [BACKUP.md](BACKUP.md).
- **Tuner** down to 38 Hz (7- and 8-string drop tunings, bass; analysis only while the tuner page is open, optional mute) and
  **looper** (record, overdub layers, undo, 30 s max) after the effects.
  On the tempo of the preset (tap tempo on the page too, MIDI clock) and a
  bar of 2 to 12 beats (6/8, 9/8 and 12/8 click in eighth notes), the
  looper can count one bar in before a recording, or before an overdub
  started from a stop (a click on each beat, higher on the first; it starts
  on the next downbeat; any transport key during the count-in cancels it),
  and quantize the first recording: stopping it carries on to the end of
  the bar, so the loop is a whole number of bars, or ends on the bar line
  just crossed when the stop comes within an eighth of a beat (100 ms at
  most) after it. A metronome click can run while recording, playing and
  overdubbing (the bars are then always quantized). The click has its own
  level, is heard on the main mix and never recorded. The page shows the
  bar and beat large, and the loop's waveform with its bar lines and the
  playhead. Overdubs keep the length; Loop ×2 doubles it (the loop plays
  twice, later overdubs span both), up to 30 s. ½ Speed plays the loop an
  octave down at half speed (MIDI CC66 too); overdubs go back to full
  speed. Saved loops: the loop as it sounds (all layers mixed) is saved
  under a name (the date and time by default) as a WAV file with its bar
  grid in `loops/` of the data directory, and loaded back or deleted from
  the Loops list; a WAV file copied there is listed too, as a free-length
  loop. The count-in, quantize and click settings are kept with the
  session.
- **Resource monitor**: the top bar shows the DSP load (time spent in the
  audio callback vs. the buffer period) and the SoC temperature, coloured
  green / orange / red. Tapping it opens the details: DSP average and peak,
  JACK load, CPU (total and busiest core), memory, dropouts (xruns), latency
  (input to output, and what the effects add: "6.3 ms · fx 1.0")
  and, on the Pi, under-voltage / throttling. Adding an effect while the load
  is already high shows a warning. The block editor shows the share of a
  CPU core the block uses, and its latency when it adds some.
- **MIDI** (feature `midi`, default): every USB / DIN MIDI input is heard,
  hot-plugged. Helix-compatible map: Program Change and CC0 banks for
  presets (or setlist slots), CC69 scenes, CC64 tap, CC68 tuner, CC49–56
  footswitches, CC60–63 and CC66 looper; MIDI clock sets the tempo; MIDI learn on any
  block parameter or on / off (expression pedals) with a heel / toe range,
  saved with the preset; notes and free CCs go to LV2 plugins taking MIDI.
  See [MIDI.md](MIDI.md).
- **Hardware** (feature `gpio`): footswitches follow the Live page mode
  (act when pressed; the tap switch also reads the hold), and keys `1`–`8`
  and `t` do the same on a keyboard. The rotary encoder drives the UI
  without the touch screen (arrows, Return and Escape on a keyboard): turn
  to move, press to open or adjust, hold to go back. In the rack it picks a
  block and opens it, or, past the last block, the add button (the browser
  then adds the focused effect); in the editor it goes through the parameters, On/Off
  and Remove (press twice; while arranging, a press picks a control and
  the turns move it, or puts it in or out of the strip), a press makes the turns change the parameter
  (in steps of 1/100 of its range, per octave for frequencies, one option
  for lists; a toggle flips at once), another press keeps the value and
  holding goes back to the value before. Presets: turn to pick, press to
  load. Live: the switches (a press acts like the footswitch), then the
  tempo (press, then turn), tap and the mode. Looper: Record, Overdub,
  Play, Stop, Undo, Clear, Loop ×2, ½ Speed, Loops, the tempo, tap, the
  bar, count-in, quantize, the click, the click level and the level. Tuner: mute and the reference
  pitch. Holding on a main page opens a page switcher (Rack, Presets,
  Live, Tuner, Looper, Settings). Settings → Hardware sets what is wired: from
  no footswitch to 8, the GPIO of each (with a LED that lights while it is
  pressed, to check the wiring), the encoder or none, its push button or
  none, its steps per detent. Kept in `hardware.json` in the data
  directory; changes apply at once. The inputs are taken through the
  kernel (GPIO character device): one on a GPIO a driver holds, such as an
  audio HAT's I2S lines (18–21, or 18–27 on 8-channel HATs), is left out
  and Settings → Hardware says so, so the default pins never cut a HAT's
  outputs.
- **External encoders** (feature `gpio`): encoder boards on the I2C bus
  (GPIO 2 and 3), one per knob of the rack's strip: Adafruit I2C rotary
  encoders (seesaw, addresses 0x36–0x3D), the Adafruit Quad (0x49–0x50) or
  the M5Stack 8Encoder (0x41); several boards of the same kind chain, in
  address order. Encoders 1–5 turn the knobs of the strip's page shown;
  pressing swaps a knob with its value before the turns (A/B), holding puts
  it back to its default; a switch flips; a list opens on a press, the turns
  then move through it and a press picks (holding closes it); the factory
  presets and model fields open on a press or a turn. A 6th encoder
  turns the strip's pages. Settings → Hardware: the board, counts per
  detent, reverse; it says which addresses answered. Without boards, keys
  `a s d f g h` turn encoders 1–6 (shifted: the other way) and `z x c v b n`
  press them (shifted: hold), on the rack page.
- **OS image** (`os/`): Buildroot image for the Pi 5 and x86_64 mini PCs,
  `PREEMPT_RT` kernel, read-only system, boot logo, the UI full screen
  without X11 / Wayland, signed A/B updates (RAUC) that roll back when the
  new system does not start. See [OS.md](OS.md).
- **Updates** (feature `update`, default): ⋮ → Updates checks the
  latest GitHub release and, on the OS image, installs it: the app alone
  when the system did not change, else the whole system. See
  [Updates](https://docs.openrig.eu/setup/updates/).

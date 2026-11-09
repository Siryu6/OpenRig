# MIDI

The pedalboard listens to every MIDI input it finds: USB controllers, the
DIN input of an audio interface (e.g. PreSonus AudioBox), a DAW. Devices
can be plugged in or out while it runs; there is nothing to select
("Midi Through" and the pedalboard's own ports are ignored).
Settings → MIDI shows the devices, an activity LED, the channel and the
clock option.

Nothing has to be configured on the pedalboard side: it follows the map
most floor units share, so a MIDI controller set up for a Helix (or a
HeadRush, a Quad Cortex, a Boss…) works as is.

## Conventions followed

| Convention | Source |
|---|---|
| Program Change selects a preset, CC0 / CC32 Bank Select | MIDI 1.0 specification, every unit |
| Banks of 128 presets with CC0 | Quad Cortex, Boss GT-1000, Fractal |
| CC1 / CC2 / CC3 for expression pedals | Helix / HX, Quad Cortex (learnable, not fixed) |
| CC49–56 footswitches, CC64 tap, CC68 tuner, CC69 scenes, CC71 modes, CC72 presets | Line 6 Helix / HX (also HeadRush) |
| CC60–63, CC66 looper | Line 6 Helix / HX |
| MIDI clock (24 ppqn) drives the tempo | MIDI 1.0 specification |
| CC learn on any parameter, saved with the preset | Helix controller assign, Quad Cortex, Kemper |
| Omni or channel 1–16 | MIDI 1.0 specification |

## Fixed map

| Message | Action |
|---|---|
| PC n | Setlist loaded: slot n+1 of the setlist. Otherwise: preset n+1 of the group (presets sorted by name) |
| CC0 n | Preset group: PC n then loads preset n × 128 + PC |
| CC32 0 | Leave the setlist (PC selects presets again) |
| CC32 n | Load setlist n, counting from 1 (setlists sorted by name) |
| CC49–56 | Footswitch 1–8, as pressed in the current Live mode |
| CC60 | Looper: 0–63 overdub, 64–127 record |
| CC61 | Looper: 0–63 stop, 64–127 play |
| CC63 | Looper: undo (64–127) |
| CC64 | Tap tempo (64–127) |
| CC66 | Looper: 0–63 full speed, 64–127 half speed |
| CC68 | Tuner on / off |
| CC69 | Scene: 0–7 scene 1–8, 8 next, 9 previous (scenes in use, wrapping around) |
| CC71 | Footswitch mode: 0 Stomp, 2 Setlist, 3 Scenes, 4 next, 5 previous (in the Live page order) |
| CC72 | Preset: 0–63 previous, 64–127 next (setlist slots when a setlist is loaded) |

Loading the preset or the slot already active does nothing, so a controller
that resends its PC does not interrupt tails.

CC0, CC32, CC49–72 and CC120–127 (channel mode) are reserved: they cannot be
learned.

## Learning a controller

1. Open a block (double tap in the rack) and tap **MIDI**.
2. Touch a control (a knob, a switch, or On / Off), then move a pedal or knob
   of your controller. Moving first and touching then works too.
3. The control shows a small "CC n" tag. Touch another control, or **Done**.

One controller can drive several controls (an expression pedal opening a wah
and closing a volume); a control follows one controller. **Clear** forgets
the controller of the touched control. Assignments are saved with the preset
(and the session).

By default, parameters follow the whole range of the controller (0–127 over
the parameter range; switches and lists by steps). On / Off: 0–63 off,
64–127 on.

By convention, use CC1–CC3 for expression pedals and CC11 for a volume pedal.

### Range (heel / toe)

The learned control stays shown in the banner with **HEEL** and **TOE**: where
the controller puts it at 0 and at 127, in percent of its range (5 % steps),
as Helix's Min / Max. A toe below the heel reverses the pedal (**Swap**), e.g.
a volume closing as the pedal opens. While the banner shows it, moving that
controller plays the control as usual, to try the range. The range is kept
when the control learns another controller.

## MIDI to LV2 plugins

LV2 plugins with a MIDI input (synths, MIDI-controlled effects) get the notes,
pitch bend, pressure and the CCs of the channel listened to that are not in
the fixed map. Program Change stays with the presets. Every MIDI plugin of
the chain gets the same messages; a bypassed block drops them.

## Clock

With **Follow MIDI clock** on (the default), an incoming clock sets the
tempo, averaged over one beat, to 0.5 BPM. The Live page shows **MIDI CLOCK**
above the tempo while the clock is running. When it stops (1 s without a
tick), the tempo is back in the player's hands. Start / Stop / Continue are
ignored.

## Channel

**Omni** (the default) listens to all channels; otherwise only the chosen
one. The clock has no channel and is always heard.

## Network MIDI

On the pedalboard OS, a computer on the same network can play the pedal
without a cable, over RTP-MIDI (AppleMIDI), the protocol of macOS's Network
MIDI. Settings → MIDI → **Network MIDI** switches it on (it is off by
default) and names the session: the host name (`pedalboard`) unless set,
up to 32 letters, digits, spaces, dots, dashes or underscores.

Once on, the pedal is announced over mDNS/Bonjour (`_apple-midi._udp`) and
listens on UDP 5004 (control) and 5005 (MIDI). On the computer:

- **macOS**: Audio MIDI Setup → Window → Show MIDI Studio → Network (the
  globe). The pedal shows in Directory; select it, Connect. The session
  ("Session 1") is then a MIDI port in Logic, Ableton, MainStage…
- **Windows**: [rtpMIDI](https://www.tobias-erichsen.de/software/rtpmidi.html)
  by Tobias Erichsen: add a session, pick the pedal in Directory, Connect.
  The session is a MIDI port for the DAW (Reaper, Cubase…).
- **Linux**: [rtpmidid](https://github.com/davidmoreno/rtpmidid). The pedal
  appears as an ALSA sequencer port of rtpmidid; connecting to it (Reaper's
  MIDI output, `aconnect`, Qsynth…) opens the session. Without mDNS:
  `rtpmidid-cli connect pedalboard <address> 5004`.

The messages go through the same path as a cable: the [fixed map](#fixed-map),
the controls learned in the block editor, the channel, the clock and the
LV2 plugins apply unchanged. Each session shows in the device list as
"<name> (network)".

The card shows what is announced, the computers connected (name and
address) and the devices allowed or blocked. The pedal sends nothing to the
network: no MIDI output, and none of its own devices is exported.

### Security

RTP-MIDI has no authentication: anyone who can reach UDP 5004 can open a
session, and a session can change presets, scenes, parameters, the tuner…
So:

- **Off by default.** Off, rtpmidid and avahi-daemon do not run: nothing
  listens.
- **New devices are asked about.** The first time a computer opens a
  session, the screen asks "Allow <name> (<address>) to control OpenRig?".
  Until the answer, its MIDI is ignored (the app does not listen to its
  port). Allow and Block are remembered by session name and address in
  `network-midi.json` (data directory), listed in the card with Remove.
  Block (or a blocked device coming back) drops the session in rtpmidid.
  Tapping outside the question leaves it waiting in the card. "Allow anyone
  on the network" skips the question; blocked devices stay blocked.
- **Local network only.** A session from an address that is not private
  (10/8, 172.16/12, 192.168/16, fc00::/7) or link-local (169.254/16,
  fe80::/10) is dropped without asking, whatever the mode. The image has no
  firewall: rtpmidid answers the protocol on any interface, and the app
  enforces this by not listening.
- mDNS (avahi) is limited to the wired and Wi-Fi interfaces and announces
  neither the machine type nor a workstation service.
- `pedalboard-report` lists the state, the sessions and the devices allowed
  or blocked (names and addresses; there are no secrets).

Limits, for the threat model: name and address are what the computer
claims; on the same network a device can take the name and address of an
allowed one (spoofing UDP from the local network is easy). The question
guards against a stranger on a shared network (a venue's Wi-Fi) taking over
the pedal by mistake or on purpose, not against an attacker in control of
the local network: on such a network, keep network MIDI off, or use a
network of your own. A blocked computer sees its session drop only when it
times out (rtpmidid has no way to say goodbye to a removed session).

## Platforms

MIDI uses midir (feature `midi`, default): ALSA sequencer on Linux
(`libasound2-dev`), CoreMIDI on macOS, WinMM on Windows. Without the feature,
or when the system has no MIDI, the Settings card says so and the rest works
as usual.

## Not supported yet

- MIDI output (sending PC to other gear, MIDI thru), over the network
  too.
- MIDI routing per block (all MIDI plugins get the same messages).
- CC62, CC65 and CC67 of the Helix map (looper play once / reverse, snapshot
  modes), Start / Stop transport.

# Live: scenes, setlists, footswitches and tempo

Three levels, as on a Helix or a Quad Cortex:

| Level | What it holds | Switch time |
|-------|---------------|-------------|
| **Preset** | The whole chain: blocks, splits, parameters, colours, its scenes and its tempo | 20 ms crossfade |
| **Scene** (in a preset) | Bypass, parameters and split settings of the blocks of the preset | Instant, no rebuild, tails ring on |
| **Setlist** | 8 preset slots for a gig, all preloaded in memory | 20 ms crossfade |

## Scenes

A preset has 8 scenes. A scene stores, for every block of the chain, whether
it is on, its parameter values, and for each split the A/B side, the mix and
the pans. It does not change the blocks themselves: adding, removing or
moving a block is done on the preset and applies to every scene.

- The current scene is always being edited: leaving it saves it, so a tweak
  made in the rack stays with the scene where it was made.
- Selecting an empty scene copies the current one into it. Build the verse,
  select scene 2, turn the drive on: that is the chorus.
- Switching scenes sends only the differences to the audio thread as live
  commands (bypass with its 10 ms ramp, parameters, A/B, mix). The chain is
  not rebuilt, so delays and reverbs keep ringing. The one exception: in
  stereo mode, moving a Mix split's pan away from or back to the centre
  switches that split between mono and stereo output, which needs a rebuild;
  and a change of more than 512 commands rebuilds the chain instead.
- Scenes are renamed from the Live page (Rename in Scenes mode, on-screen
  keyboard). The
  top bar shows the current scene in amber above the preset name.
- A preset whose scenes were never used saves none; older presets load with
  scene 1 only.

## Setlists

A setlist (formerly called "scene") maps up to 8 footswitch slots to
presets. Loading it preloads every preset, so changing song is a crossfade
with no disk access. Setlists are managed on the Presets page (Presets /
Setlists / Slots) and are stored as `scenes/<id>.json` in the data
directory:

```json
{
  "id": "setlist-uuid",
  "name": "Blues Night",
  "description": "",
  "preset_slots": ["preset-uuid-1", "preset-uuid-2", null, null, null, null, null, null]
}
```

## Footswitches

The Live page shows the 8 switches as tiles; tapping a tile does what the
switch does. The mode is chosen on that page and kept across restarts:

| Mode | Switch n | LED |
|------|----------|-----|
| **Stomp** (default) | Turns a block on or off | Block colour, lit when on |
| **Scenes** | Recalls scene n of the preset | Amber on the current scene |
| **Setlist** | Loads preset n of the setlist | Amber on the loaded slot |

In Stomp mode each switch follows the chain order (switch 1 = first block,
and so on), unless a block was assigned to it: **Assign**, then tap a tile
and pick the block, or "Automatic" to go back to the chain order.
Assignments are saved in the preset.

Switches act when pressed, not on release, so a scene change lands on the
beat.

**Tap switch**: with two switches or more, the last physical switch (FS 4
with the four default GPIOs) taps the tempo instead by default. Held for
0.8 s it opens the tuner; pressing any switch then closes it and goes back
to the previous page. It can be
turned off in Settings, which gives the switch back to the mode.

Keyboard (USB keyboard, or the desktop build): keys `1`–`8` are switches 1
to 8 (with the same hold on the tap switch), `t` taps the tempo.

## Tempo

Each preset has a tempo (40–240 BPM, 120 by default), shown on the Live page
and in the top bar. It is set with the TAP key (or the tap switch: the
average of the last four intervals between taps, restarting after a 2 s pause), or with − / +.

The tempo is sent to every LV2 control port designated
`time:beatsPerMinute`, or whose unit is BPM, and new blocks start at the
current tempo. Plugins that follow the host transport get it as a
`time:Position` event (always rolling, 4/4) on every tempo change and when
the chain is built. A MIDI clock can drive the tempo too (see
[MIDI.md](MIDI.md)).

## Tails (spillover)

Delay and reverb blocks (LV2 plugins of those categories and the Space
block) keep ringing when they are bypassed: their input is faded out while
their output keeps playing, until it has been silent for half a second.
Other blocks are bypassed with a 10 ms ramp. Tails survive scene changes and
footswitch bypasses; a preset change or a structural edit crossfades to a new
chain, which ends them over 20 ms.

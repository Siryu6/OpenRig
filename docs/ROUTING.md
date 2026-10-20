# Inputs, outputs, sends and returns

How OpenRig uses an interface with more than one input and two outputs. The
[user guide](https://openrig.eu/docs/setup/routing/) shows the Routing page;
this page covers the signal path and the files.

## Ports

The pedalboard registers as many JACK ports as the interface has, at least
one input and two outputs, at most 16 of each: `pedalboard:in_N` is
connected to capture port N, `pedalboard:out_N` to playback port N. `in_1`,
`out_1` and `out_2` keep their former names as aliases (`guitar_in`,
`out_left`, `out_right`).

The ports the interface offers and each connection made are logged at
start (`Interface inputs`, `Interface outputs`, `Connected`), and a
connection that fails is logged as a warning: see
[OS.md, Troubleshooting](OS.md#troubleshooting) for a silent output.

## Signal path

In the JACK callback, for each period:

1. The **effects input** (input 1 by default) goes to the tuner, which may
   mute it.
2. The **chain** processes it. A **Return** block mixes an interface input
   into the signal at its place (at 100% mix, the default, the input
   replaces it); a **Send** block adds the signal at its place to one of 4
   **send buses**, while the signal carries on. Both are mono or stereo like
   the path at that point.
3. The **looper** plays and records on the main mix only.
4. The **output router** fills each output from the main mix, a send bus
   or an input passed straight through, with level, balance, mute, solo and
   polarity. Every change is ramped over 10 ms; a change of source fades the
   old one out, then the new one in.

While the tuner mutes, the returns and the inputs passed straight through
are silent too. During a preset crossfade, the old chain's sends fade out
with its sound and the new chain's sends fade in. A send in the unselected
branch of an A/B split is silent, as that branch is not processed. Level,
mix and bypass changes of a send or return are ramped over 10 ms; a change
of bus or input fades out, switches, then fades in.

All of this runs without allocation or locks: the inputs and buses are
arrays on the stack or buffers sized when JACK's buffer size changes, and
new routings reach the audio thread through a bounded channel.

## routing.json

The outputs belong to the rig, not to a preset: they are kept in
`routing.json` in the data directory, written atomically one second after
the last change (and when the app quits). Without the file, input 1 feeds
the chain and the main mix plays on outputs 1 and 2.

```json
{
  "input": 0,
  "strips": [
    { "port": 0, "stereo": true, "source": "main",
      "gain_db": 0.0, "pan": 0.0, "mute": false, "solo": false, "invert": false },
    { "port": 2, "stereo": false, "source": { "send": 0 }, "fold": "sum",
      "gain_db": -3.0, "pan": 0.0, "mute": false, "solo": false, "invert": false },
    { "port": 3, "stereo": false, "source": { "input": 1 },
      "gain_db": 0.0, "pan": 0.0, "mute": false, "solo": false, "invert": false }
  ]
}
```

| Field | Meaning |
|---|---|
| `input` | Effects input, from 0 |
| `port` | First output of the strip, from 0 |
| `stereo` | The strip plays on `port` and `port + 1` |
| `source` | `"main"`, `{ "send": n }` (0 to 3) or `{ "input": n }` |
| `fold` | A stereo source on a single output: `"sum"`, `"left"` or `"right"` |
| `gain_db` | −60 to +12 dB |
| `pan` | Balance of a stereo strip, or where an input sits in it, −1 (left) to 1 (right) |
| `solo` | While any strip is soloed, only soloed strips play |
| `invert` | Flip the polarity |

Only `port`, `stereo` and `source` are required; the other fields default
to 0 or off, and `fold` to `"sum"`.

An output in no strip is silent. A file with two strips on the same output,
an input or send that cannot exist, or a level that is not a number is
ignored with a warning, and the default routing is used.

## Send and return blocks

Sends and returns are built-in blocks, added from **+** like any other
(family **Routing**), saved with the preset and its scenes:

| Block | URI | Parameters, in order |
|---|---|---|
| Send | `urn:pedalboard:send` | Send bus (0 to 3), Level (−60 to +12 dB) |
| Return | `urn:pedalboard:return` | Input (0 to 15, default 1), Level (dB), Mix (0 to 100 %, default 100) |

A bypassed send stops adding to its bus; a bypassed return lets the signal
through. Scenes can switch them like any block, so a song part can send to
another output or bring another instrument in. A return on an input the
interface does not have plays silence (scaled by its mix), so a preset made
on a larger interface still loads.

## Code

- `src/audio/routing.rs`: `Routing`, `Strip`, and `OutputRouter` (the
  audio-thread side).
- `src/plugins/scanner.rs`: the Send and Return blocks (`SEND_URI`,
  `RETURN_URI`, their parameters).
- `src/plugins/chain.rs`: `ChainIo`, and `RouteProc` that runs those
  blocks.
- `src/audio/jack_client.rs`: the ports and the callback.
- `src/routing_ui.rs` and `ui/pages/routing_view.slint`: the Routing page.

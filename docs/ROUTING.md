# Inputs, outputs, sends and returns

How OpenRig uses an interface with more than one input and two outputs. The
[user guide](https://docs.openrig.eu/setup/routing/) shows the Routing page;
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
5. Each output or stereo pair then goes through its **EQ** and its
   **limiter**, after the level and before the meters. A stereo pair shares
   one limiter, so the image does not move.

While the tuner mutes, the returns and the inputs passed straight through
are silent too. During a preset crossfade, the old chain's sends fade out
with its sound and the new chain's sends fade in. A send in the unselected
branch of an A/B split is silent, as that branch is not processed. Level,
mix and bypass changes of a send or return are ramped over 10 ms; a change
of bus or input fades out, switches, then fades in.

## Output EQ and limiter

They set how the rig sounds on a given system, whatever the preset. Open
them from **EQ** or **LIMIT** on an output card.

- The **EQ** has 6 bands: low cut, low shelf, mid 1, mid 2, high shelf and
  high cut. The cuts are 12 dB/octave. The responses are those of the RBJ
  cookbook filters, run as state-variable filters so that changes are
  smooth. Each band can be turned off; the EQ is off by default.
- The **limiter** keeps every sample under the ceiling (−1 dBFS by
  default, −12 to 0 dB). It looks 1 ms ahead, so a peak is caught before
  it plays, with no clipping. The release (100 ms by default, 10 to
  1000 ms) sets how fast the level comes back. It is on by default.
- The limiter works on sample peaks. A converter can still make peaks a
  little higher between samples, so −1 dB leaves room for them.
- While the limiter is on, the output is 1 ms late. Outputs without a
  limiter are not delayed, so two outputs can be 1 ms apart. JACK knows
  each output's latency (see [Latency](#latency)), so a recorder can
  realign them.
- The **GR** meter under each output shows how much the limiter takes off.
- Turning the EQ, a band or the limiter on or off fades over 10 ms.

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
      "gain_db": 0.0, "pan": 0.0, "mute": false, "solo": false, "invert": false,
      "eq": { "on": true,
              "low_cut": { "on": true, "freq": 80.0 },
              "mid_1": { "on": true, "freq": 400.0, "gain_db": -3.0, "q": 1.4 } },
      "limiter": { "on": true, "ceiling_db": -1.0, "release_ms": 100.0 } }
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
| `eq.on` | The EQ of the strip is on |
| `eq.<band>` | `low_cut`, `low_shelf`, `mid_1`, `mid_2`, `high_shelf`, `high_cut`: `on`, `freq` (20 to 20000 Hz), `gain_db` (−12 to +12, shelves and mids), `q` (0.3 to 8, mids) |
| `limiter.on` | The limiter of the strip is on |
| `limiter.ceiling_db` | −12 to 0 dB |
| `limiter.release_ms` | 10 to 1000 ms |

Only `port`, `stereo` and `source` are required; the other fields default
to 0 or off, and `fold` to `"sum"`. A missing `eq` or band takes its
default: the EQ off, every band flat. A missing `limiter` is on at −1 dB: a file from an
older version loads with the limiter on.

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

## Latency

Some blocks are late: an LV2 plugin reports how many frames on its
`lv2:reportsLatency` control output (a lookahead compressor, a pitch
shifter, a linear-phase EQ), and the limiter looks 1 ms ahead. The value is
read after every period, so a plugin can change it at any time.

Wherever two paths meet again, the earlier one is delayed to match:

- **Splits.** The branches of a Mix or L/R split arrive together, so a
  wet/dry mix has no comb filtering. An A/B split is as late as its latest
  branch, whichever is selected, so switching never shifts the timing.
- **Bypass.** A late block that is bypassed keeps its latency: its dry
  signal is delayed by as much. Bypassing does not move the sound in time,
  and the bypass crossfade does not comb.
- **Returns.** A Return delays its input by the latency of the chain before
  it, so the input meets the signal at its place. After a Send on the same
  path (an effect loop), only the latency since that Send is added.
- **Sends** are not delayed. Each bus has a latency range, reported to
  JACK with the output that plays it.

Each delay line holds up to 100 ms and is allocated when the chain is
built; a chain without late blocks has none. A change of delay crossfades
from the old to the new delay over 10 ms, so it never clicks. Presets made
before need no change.

The latency is reported to JACK (`jack_port_set_latency_range`, in the
latency callback): each output adds its effects, send or limiter to the
latency of the input it comes from, and the effects input reports the
outputs it feeds. A change is passed on within 250 ms. The DSP panel shows
the latency from input to output, JACK periods included, and what the
effects add (`6.3 ms · fx 1.0`); the block editor shows a block's latency
when it has some.

## Code

- `src/audio/routing.rs`: `Routing`, `Strip`, and `OutputRouter` (the
  audio-thread side).
- `src/audio/master.rs`: the output EQ and limiter (`OutputEq`, `Limiter`,
  `MasterProc`).
- `src/plugins/scanner.rs`: the Send and Return blocks (`SEND_URI`,
  `RETURN_URI`, their parameters).
- `src/plugins/chain.rs`: `ChainIo`, and `RouteProc` that runs those
  blocks.
- `src/audio/latency.rs`: `DelayLine`, and the latency reports
  (`LatencyReport`, `BlockLatency`).
- `src/audio/jack_client.rs`: the ports, the callback and the JACK
  latency callback.
- `src/routing_ui.rs` and `ui/pages/routing_view.slint`: the Routing page.

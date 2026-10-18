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

## Signal path

In the JACK callback, for each period:

1. The **effects input** (input 1 by default) goes to the tuner, which may
   mute it.
2. The **chain** processes it. A block with a **return** has its input
   replaced by an interface input just before it; a block with a **send**
   adds its output to one of 4 **send buses** just after it, while the
   signal carries on. Returns are mono or stereo like the path at that
   point, and so are sends.
3. The **looper** plays and records on the main mix only.
4. The **output router** fills each output from the main mix, a send bus
   or an input passed straight through, with level, balance, mute, solo and
   polarity. Every change is ramped over 10 ms; a change of source fades the
   old one out, then the new one in.

While the tuner mutes, the returns and the inputs passed straight through
are silent too. During a preset crossfade, the old chain's sends fade out
with its sound and the new chain's sends fade in. A send in the unselected
branch of an A/B split is silent, as that branch is not processed.

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

## Sends and returns in a preset

Sends and returns are part of the preset (and of the session), keyed by the
block's instance id, in `chain.io`:

```json
"io": {
  "0": { "send": 0 },
  "3": { "input": 1 }
}
```

`input` is the return before the block, `send` the send after it, both from
0; either can be left out. Removing a block removes its entry. A return on
an input the interface does not have plays silence, so a preset made on a
larger interface still loads.

## Code

- `src/audio/routing.rs`: `Routing`, `Strip`, `BlockIo`, and `OutputRouter`
  (the audio-thread side).
- `src/plugins/chain.rs`: `ChainIo`, and the `Return` / `Send` nodes built
  around the blocks that have them.
- `src/audio/jack_client.rs`: the ports and the callback.
- `src/routing_ui.rs` and `ui/pages/routing_view.slint`: the Routing page.

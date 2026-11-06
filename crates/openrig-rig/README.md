# openrig-rig

The shareable **rig** format of [OpenRig](https://github.com/Siryu6/OpenRig)
(`.openrig` files), and its reference reader / writer in Rust.

A rig is one OpenRig preset (the whole chain: blocks, splits, parameters,
scenes, tempo, footswitch assignments) together with a **dependency
manifest** saying where each plugin and each model file it uses comes from.
Another pedalboard rebuilds the rig from it: plugins from the OpenRig store,
models from TONE3000 with the user's own account, blocks built into OpenRig.
A rig never embeds plugins or models, only references to them.

The crate depends on `serde`, `serde_json` and `thiserror` only, has no
platform code and builds for any target. The pedalboard and the community
site both use it, so it is the contract between them.

```toml
[dependencies]
openrig-rig = { git = "https://github.com/Siryu6/OpenRig", package = "openrig-rig" }
```

## File

| | |
|---|---|
| Extension | `.openrig` |
| Media type | `application/vnd.openrig.rig+json` (`MEDIA_TYPE`) |
| Encoding | UTF-8 JSON object, written indented |
| Size | at most 1 MiB (`MAX_RIG_BYTES`); real rigs are a few KB |

Example: [`fixtures/plexi-crunch.openrig`](fixtures/plexi-crunch.openrig).

```json
{
  "format": 1,
  "meta": { … },
  "preset": { … },
  "dependencies": { "plugins": [ … ], "files": [ … ] },
  "display": { "blocks": { … } }
}
```

| Field | | |
|---|---|---|
| `format` | integer | Required |
| `meta` | object | Required (its `name` is) |
| `preset` | object | Required |
| `dependencies` | object | Optional: absent reads as empty, which only suits a chain without blocks |
| `display` | object | Optional, added after the first rigs: absent in older files |

Conventions for the whole file:

- **Characters** in the limits below are Unicode scalar values (Rust
  `char`s), not bytes; byte limits say bytes.
- **Instance ids** (`instance_id`, `blocks`, `stomps`) are unsigned
  integers. Used as object keys (`chain.files`, `chain.colors`,
  `display.blocks`…) they are the same numbers written as decimal
  strings: `"3"`.
- An **optional** field may be absent; a string field that is absent reads
  as `""`, a list as `[]`. Writers may leave out empty optional fields.

### `format`

Integer, required. `1` for this version (`FORMAT_VERSION`). A reader refuses
a file without it, with `0`, or with a number higher than the one it knows
(`Error::UnsupportedFormat`), before reading anything else.

### `meta`

| Field | Type | |
|---|---|---|
| `name` | string | Required, 1–80 characters, not only spaces. Name of the rig, and of the preset created on import |
| `description` | string | Optional, up to 4000 characters |
| `tags` | string[] | Optional, up to 16 tags of 1–32 characters, not only spaces. OpenRig writes them trimmed and lower case |
| `author` | string | Optional, up to 200 characters: the name the exporting user set on their pedalboard, free text and unverified. A site may show its own account name instead |
| `app_version` | string | Optional, up to 200 characters: OpenRig version that exported it, `"0.11.0"` |
| `created` | string | Optional, up to 200 characters: export time, RFC 3339 in UTC (`"2026-11-06T18:30:00Z"`); empty or absent when the clock was not set |

### `preset`

The OpenRig preset exactly as the app saves it in `<data>/presets/*.json`,
carried as raw JSON (`serde_json::Value`): the app adds preset fields over
time, and a rig must keep them without a new release of this crate. The
crate reads it through a typed, read-only view, `Rig::chain()`
(`ChainView`), which never fails: missing fields read as defaults, nodes of
an unknown kind as `Block::Unknown`.

Fields the view reads (everything else is carried untouched):

| Path | |
|---|---|
| `name` | Preset name |
| `stereo_mode` | Stereo processing |
| `chain.nodes` | The chain tree, in signal order. Each node is `{"Plugin": {…}}` or `{"Split": {…}}` |
| `Plugin.instance_id` | Id of the block; key of its entries in the maps below |
| `Plugin.uri` | Plugin URI: `urn:pedalboard:…` for built-in blocks, the LV2 URI otherwise |
| `Plugin.enabled` | `false` when bypassed |
| `Plugin.num_inputs` / `num_outputs` | 1 mono, 2 stereo |
| `Split.split_type` | `"Ab"` (one branch heard), `"Mix"` (blended), `"Lr"` (top left, bottom right) |
| `Split.top_chain` / `bottom_chain` | Branches A and B, nodes again |
| `Split.ab_select_a`, `mix_ratio`, `enabled` | Split settings |
| `chain.files` | `{"<instance_id>": "<path>"}`: the model or impulse response of a built-in block |
| `chain.param_files` | `{"<instance_id>": {"<parameter URI>": "<path>"}}`: files of LV2 plugins |
| `chain.colors` | `{"<instance_id>": "<colour name>"}`: colours the user chose; a name outside the palette (see [`display`](#display)) is ignored by the app. The colour actually drawn is in `display` |
| `chain.scenes` | Scene slots: `null` (never used) or an object with a `name` |
| `chain.scene` | Slot the preset opens on |
| `chain.bpm` | Tempo (120 when absent) |
| `chain.stomps` | Footswitch slots in stomp mode: instance id or `null` |
| `chain.midi` | Learned MIDI CCs (OpenRig leaves them out of exports unless asked) |

Parameter values are in `chain.params_by_symbol`
(`{"<instance_id>": {"<port symbol>": value}}`) and `chain.params` (by
port position, for older apps). Very old presets have no `chain`, only a
flat `plugins` list (`uri`, `name`, `bypassed`); the view reads it too.

All file paths are **relative to the models directory**, `/`-separated.

### `dependencies`

```json
"dependencies": {
  "plugins": [ { "uri": "…", "name": "…", "source": { "type": "…", … } } ],
  "files":   [ { "path": "…", "blocks": [2], "source": { "type": "…", … } } ]
}
```

- `plugins`: one entry per plugin URI the chain uses (listed once even when
  several blocks use it).
- `files`: one entry per path in `chain.files` and `chain.param_files`.

| Field | Type | |
|---|---|---|
| `plugins[].uri` | string | Required, up to 400 characters: the URI as in the chain |
| `plugins[].name` | string | Optional, up to 200 characters: display name ("Amp", "ZamComp") |
| `plugins[].source` | object | Required, see below |
| `files[].path` | string | Required: the path as in the chain, safe (see [Paths](#paths)) |
| `files[].blocks` | integer[] | Optional: instance ids of the blocks that load it |
| `files[].source` | object | Required, see below |

`source` is one of:

| `type` | Fields | For | Meaning |
|---|---|---|---|
| `builtin` | `kind` | plugins | Block built into OpenRig; `kind` (required, non-empty) is the end of its URI (`nam`, `nam-amp-cab`, `nam-pedal`, `ir`, `ir-space`, `send`, `return`) |
| `store` | `package`, `version`, `plugin_id`? | plugins | LV2 plugin of the [OpenRig catalog](https://github.com/Siryu6/openrig-catalog): package id and version installed on the exporting board (both required, non-empty), and the catalog entry id (optional). On import, the current version of the package is installed (settings are kept by port symbol) |
| `tone3000` | `tone_id`, `model_name`, `model_id`?, `tone_title`?, `url`?, `license`?, `creator`?, `gear`?, `format`? | files | A model or IR from [TONE3000](https://www.tone3000.com). Required: `tone_id` (not 0) and `model_name` (non-empty); the texts are up to 400 characters. `tone_id` and `model_id` are TONE3000 API ids: `GET /api/v1/tones/{tone_id}`, `GET /api/v1/models/{model_id}`, whose `model_url` downloads the file with the user's token. `gear` is the TONE3000 gear type (`amp`, `amp-cab`, `pedal`, `outboard`, `cab`, `space`, `experimental`), `format` `nam` or `ir` (OpenRig imports no other). `model_id` is absent when the exporting board did not know it: the importer then looks for `model_name` among the models of the tone. `license` is TONE3000's text (`"CC BY 4.0"`), absent when unknown |
| `local` | `what`, `name` | both | Only on the exporting board (a plugin copied by hand, a model of one's own). `what`: `plugin`, `model`, `impulse response`, `file`; `name` the plugin or file name, for people. Such a rig imports without it, and **cannot be published** |

A `type` this crate does not know (a newer format) reads as
`Source::Unknown`: treated as not available, not as an error.

### `display`

How the exporting app draws each block in its rack. OpenRig writes an
entry for **every plugin block** of every rig it exports. **Renderers
must use `kind` for the shape and `color` / `color_hex` for the colour as
written**, and never infer them from the plugin name or URI: the rig
records what its app drew.

The field is optional only for rigs written before it: for a file without
`display`, or a block without an entry, a renderer may fall back to
guessing (from `source.kind` of a built-in block, the plugin name, the
palette defaults below). That fallback is never used when the entry is
there. The view reads such missing fields as `None`.

```json
"display": {
  "blocks": {
    "2": { "kind": "amp", "category": "simulator", "color": "red", "color_hex": "#9a3a33" },
    "1": { "kind": "plugin", "category": "dynamics", "lv2_class": "CompressorPlugin",
           "color": "pink", "color_hex": "#8c4a64" }
  }
}
```

`blocks` holds one entry per **plugin block** of the chain (not splits),
keyed by instance id:

| Field | Type | |
|---|---|---|
| `kind` | string | Required. Shape in the rack: `amp` (head with its control panel), `combo` (amp with a grille: amp + cab captures), `cab` (speaker cabinet), `pedal` (stompbox), `space` (room or reverb, rounded box), `send`, `return` (jack plates), `plugin` (plain card, every other plugin). Draw a kind you do not know as `plugin` |
| `category` | string | Optional. Family of the plugin in OpenRig: `dynamics`, `distortion`, `filter`, `modulation`, `delay`, `reverb`, `simulator`, `utility`, `generator`, `routing`. Absent when the exporting board did not know it (plugin not installed there, or of no known family) |
| `lv2_class` | string | Optional. LV2 class of the plugin by its local name (`CompressorPlugin`, `ReverbPlugin`…), absent for built-in blocks and plugins without one |
| `color` | string | Required. Name of the colour the block is drawn in: the user's choice from `chain.colors` when it is one of the palette, else its family's. One of the palette below |
| `color_hex` | string | Required. The same colour, `#rrggbb` |

Texts are up to 200 characters. The palette (OpenRig's `BLOCK_COLORS`;
frames and trims of the blocks, the face of the gear is dark):

| `color` | `color_hex` | Default for |
|---|---|---|
| `red` | `#9a3a33` | `amp`, `combo`, category `simulator` |
| `orange` | `#a35a2c` | `cab` |
| `yellow` | `#9a8136` | `pedal`, category `distortion` |
| `green` | `#4e7c4a` | category `delay` |
| `teal` | `#30706e` | `space`, category `reverb` |
| `blue` | `#3e5a87` | category `modulation` |
| `purple` | `#644f86` | category `filter` |
| `pink` | `#8c4a64` | category `dynamics` |
| `silver` | `#696c72` | anything else (`utility`, `generator`, `routing`, unknown) |

The default applies the first matching row from the top of this order:
kind `amp`/`combo` → red, `cab` → orange, `pedal` or `distortion` → yellow,
`space` or `reverb` → teal, `simulator` → red, `delay` → green,
`modulation` → blue, `filter` → purple, `dynamics` → pink, else silver.
This table and order are only the fallback for files without `display`:
renderers use `color_hex` as written, since the palette may change in a
later app.

## Validation

`Rig::validate()` returns a `Report`: a list of `Issue { level, code, at,
message }` (`at` is a JSON pointer into the rig). `Report::is_valid()` and
`Report::is_publishable()` answer the two questions that matter.

| Level | Meaning |
|---|---|
| `invalid` | Refuse the file |
| `unpublishable` | Fine to import, refuse to publish |
| `warning` | Show or ignore |

| Code | Level | When |
|---|---|---|
| `unsupported_format` | invalid | `format` not known |
| `missing_name` | invalid | Empty name |
| `too_long` | invalid | A text over its limit, an empty tag |
| `too_many` | invalid | Over 16 tags, 128 nodes (blocks + splits), splits nested over 8 deep, 512 dependencies, 128 `display` entries |
| `invalid_preset` | invalid | `preset` not an object, or a node of an unknown kind |
| `unsafe_path` | invalid | A path in the preset or the manifest fails `is_safe_relative_path` |
| `missing_dependency` | invalid | A plugin URI or file path of the preset has no manifest entry |
| `duplicate_dependency` | invalid | Two entries for one URI or path |
| `bad_source` | invalid | A source that does not fit (`tone3000` plugin, `store` / `builtin` file) or lacks its required fields |
| `local_dependency` | unpublishable | A `local` source |
| `unknown_source` | unpublishable | A source of a newer format |
| `unused_dependency` | warning | A manifest entry the preset does not use |
| `missing_model_id` | warning | A `tone3000` file without `model_id` |
| `empty_chain` | warning | No block |
| `bad_display` | warning | A `display` entry for no plugin block of the chain, without `kind`, or without a `color` name and a `#rrggbb` `color_hex` (the view ignores its colour) |

The codes are serialized in `snake_case`; the enum is `#[non_exhaustive]`,
new codes may come in a minor version.

### Paths

A rig comes from someone else. A reader joins a path to its models
directory only when `is_safe_relative_path` accepts it: not empty, at most
512 bytes, `/`-separated, no leading `/`, no `\`, no `:` (drives, Windows
streams), no empty / `.` / `..` component, no control character, no
component starting with a space or ending with a dot or a space, no Windows
device name (`CON`, `NUL`, `COM1`…). The pedalboard also never writes a
downloaded model where the rig says: it computes the path itself from the
TONE3000 ids and rewrites the preset.

Nothing in a rig is ever executed.

## Versioning

- **Readers ignore unknown fields**, at every level, and read unknown
  sources as `Unknown`. New optional fields and new source types are added
  without changing `format`.
- A server that stores or serves rigs should keep the **bytes it
  received**: reading a rig into `Rig` and writing it again keeps
  `preset` whole but drops fields of `meta`, `dependencies` and `display`
  that this version of the crate does not know.
- `format` changes only when an older reader would misread a rig (a field
  changing meaning or type, a required field). Readers refuse a newer
  `format` with a clear message ("needs a newer OpenRig").
- Writers always write the current `format`.
- The crate version follows semver on its Rust API: a new `format` is a new
  minor version that still reads the older formats.

## Rust API

```rust
use openrig_rig::{Rig, Level};

let rig = Rig::from_slice(&bytes)?;          // size, format, JSON
let report = rig.validate();
if !report.is_valid() { /* reject */ }
let publishable = report.is_publishable();

let chain = rig.chain();                     // ChainView
for placed in chain.plugins() {              // PlacedBlock
    let block = placed.block;
    println!("{} {} ({})", placed.location, block.name,
             block.source.as_ref().map_or("?", |s| s.type_name()));
    // How the app draws it, None in rigs without `display`
    let shape = block.kind.as_ref().map_or("plugin", |k| k.as_str());
    let hex = block.rack_color.as_ref().map_or("#696c72", |c| c.hex.as_str());
}
let scenes = &chain.scenes;                  // Vec<Option<String>>
let bytes = rig.to_vec_pretty()?;
```

| Item | |
|---|---|
| `Rig { format, meta, preset, dependencies, display }` | The file. `Rig::new`, `from_slice`, `from_json`, `to_vec_pretty`, `chain`, `file_refs`, `validate` |
| `Meta` | `name`, `description`, `tags`, `author`, `app_version`, `created` |
| `Dependencies { plugins, files }` | `plugin(uri)`, `file(path)`, `sources()`, `shareable()` |
| `PluginDependency { uri, name, source }`, `FileDependency { path, blocks, source }` | Manifest entries |
| `Source` | `Builtin { kind }`, `Store { package, version, plugin_id }`, `Tone3000(Tone3000Model)`, `Local { what, name }`, `Unknown` |
| `RackDisplay { blocks }`, `BlockDisplay { kind, category, lv2_class, color, color_hex }` | `display` as written (strings). `RackDisplay::block(id)` |
| `ChainView` | `preset_name`, `stereo`, `blocks`, `scenes`, `current_scene`, `bpm`, `stomps`, `midi_mappings`; `plugins()`, `node_count()`, `depth()` |
| `Block` | `Plugin(PluginBlock)`, `Split(SplitBlock)`, `Unknown` |
| `PluginBlock` | `instance_id`, `uri`, `name`, `source`, `enabled`, `inputs`, `outputs`, `file`, `param_files`, `color` (the user's colour name from `chain.colors`, as written), and from `display`: `kind: Option<BlockKind>`, `category: Option<String>`, `lv2_class: Option<String>`, `rack_color: Option<BlockColor>` (the colour drawn; `None` when the entry has no name or no valid hex) |
| `BlockKind` | `Amp`, `Combo`, `Cab`, `Pedal`, `Space`, `Send`, `Return`, `Plugin`, `Other(String)`; `parse`, `as_str`; serialized as its name (`"amp"`) |
| `BlockColor` | `name`, `hex` (`#rrggbb`, lower case) |
| `SplitBlock` | `instance_id`, `kind` (`SplitKind::Ab / Mix / Lr / Other`), `enabled`, `top`, `bottom`, `select_a`, `mix` |
| `PlacedBlock` | `location` (`"3"`, `"2/A/1"`), `depth`, `block` |
| `FileRef` | `instance_id`, `param`, `path` |
| `Report`, `Issue`, `Level`, `Code` | Validation |
| `is_safe_relative_path`, `limits::*`, `FORMAT_VERSION`, `FILE_EXTENSION`, `MEDIA_TYPE`, `MAX_RIG_BYTES`, `Error` | |

`ChainView`, `Report` and the manifest types implement `Serialize`, so a
server can return them as JSON as they are.

## License

GPL-3.0-or-later, like OpenRig.

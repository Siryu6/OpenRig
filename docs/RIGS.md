# Rigs

A rig is a preset you can share: one `.openrig` file holding the chain
(blocks, parameters, splits, scenes, tempo) and the list of what it needs,
meaning the plugins and the model files with where each one comes from. On
another OpenRig, importing the rig installs what is missing and adds the
chain as a new preset.

The file format is described, versioned and validated by the
[`openrig-rig`](../crates/openrig-rig/README.md) crate. The app and the
community site both use that crate, and its README is the reference for
the format.

## On the pedalboard

Presets page → share button (next to Save) opens the Rigs panel.

- **Export** writes the current chain to `<data>/rigs/<name>.openrig`, and
  to a USB drive when one is mounted (**To USB**). MIDI CCs learned by the
  blocks fit one controller setup, so they are left out unless the
  **MIDI CCs** toggle is on. The note under the title lists what only this
  pedalboard has (see below).
- **Import** lists the `.openrig` files of `<data>/rigs` and of the top
  folder of mounted USB drives. Tap one to see what it needs:
  - **On board**: already here.
  - **Store**: a plugin the store installs.
  - **TONE3000**: a model downloaded with your TONE3000 account.
  - **Missing**: cannot be had here, so the preset is imported without it.

  **Install rig** installs and downloads what it can and keeps going when
  one item fails. It then saves the chain as a new preset, with the paths
  of this pedalboard, and loads it. Each item then shows whether it was
  installed or failed, and why.

`<data>` is `$PEDALBOARD_DATA_DIR`, else the platform data directory
(`~/.local/share/pedalboard` on Linux, `/data/pedalboard` on the OS
image).

USB drives are found where the desktop mounts them: `/media/<user>/*` and
`/run/media/<user>/*` on Linux, `/Volumes/*` on macOS. The OpenRig OS
image does not mount drives by itself yet, so there, and on Windows, copy
files to `<data>/rigs` instead.

## Where blocks come from

When a rig is exported, each plugin and file is resolved to a source:

| Source | For | How it is found |
|--------|-----|-----------------|
| `builtin` | NAM, IR, Send/Return blocks | Built into OpenRig |
| `store` | LV2 plugins listed in the [store](STORE.md) catalog | Catalog package and plugin id. Installed by hand also counts if the catalog has it |
| `tone3000` | Models downloaded from TONE3000 | Tone and model ids, from the tone's `tone.json` (or the `[id]` in the file name for older downloads) |
| `local` | Anything else: sideloaded plugins, your own models and IRs | Only the name. The rig still imports, without that item |

Model paths are relative to the models directory. A file outside it is
left out of the rig, and the export note says so.

## Safety

Importing never runs anything from the file. The rig is size-checked
(1 MiB at most) and validated by the crate before anything is installed.
Paths must be plain relative paths (no `..`, no absolute or drive paths,
no control characters). Downloads go where the app decides, not where the
rig says. Plugins come only from the signed store catalog, and models only
from TONE3000.

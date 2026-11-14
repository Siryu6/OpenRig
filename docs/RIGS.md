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
  to a USB drive when one is mounted (**To USB**, to the first drive by
  name; **Eject** next to it ejects that drive, on the OpenRig OS, before
  pulling it out). MIDI CCs learned by the
  blocks fit one controller setup, so they are left out unless the
  **MIDI CCs** toggle is on. The note under the title lists what only this
  pedalboard has (see below). **Author** sets, once, the name written in
  every rig you export; it is kept with the session (empty: anonymous).
  The rig also records how the rack draws each block (shape, family,
  colour name and hex), and the community site draws it with exactly
  those values.
- **Import** lists the `.openrig` files of `<data>/rigs` and of the top
  folder of mounted USB drives, each with its file name and export date
  under the rig name (two rigs may share a name). Tap one to see what it
  needs:
  - **On board**: already here.
  - **Store**: a plugin the store installs.
  - **TONE3000**: a model downloaded with your TONE3000 account.
  - **Missing**: cannot be had here, so the preset is imported without it.

  **Install rig** installs and downloads what it can and keeps going when
  one item fails. It then saves the chain as a new preset, with the paths
  of this pedalboard, and loads it. Each item then shows whether it was
  installed or failed, and why. Blocks left without their model say so in
  the same message as the import summary.

  A TONE3000 model the import could not get (no account, download failed)
  stays referenced by the preset: exporting it again writes its TONE3000
  source, not `local`, until the model is downloaded or replaced.

`<data>` is `$PEDALBOARD_DATA_DIR`, else the platform data directory
(`~/.local/share/pedalboard` on Linux, `/data/pedalboard` on the OS
image).

USB drives are found where the system mounts them: `/media/*` on the
OpenRig OS (see [OS.md](OS.md#usb-drives)), `/media/<user>/*` and
`/run/media/<user>/*` on Linux desktops, `/Volumes/*` on macOS. On Windows,
copy files to `<data>/rigs` instead. `PEDALBOARD_MEDIA_DIR` replaces these
places: each of its subfolders is taken as a drive. The list follows the
drives plugged in and pulled out, without Refresh (the app looks every two
seconds).

## On openrig.eu

The Rigs panel has two tabs: **This pedalboard** (export and import, as
above) and **openrig.eu**, the community site. Builds without the
`rigs-online` feature have no second tab.

- **Browse**: search (on-screen keyboard), sort (trending, new, top of
  the week, downloads), style chips and pages of 24 rigs with **Load
  more**. Each card shows the name, author, style, votes,
  downloads, and a STORE or TONE3000 badge when the rig needs them.
- **Rig detail**: description, scenes, versions and the rig's address on
  the site, with what it needs on the right (each item marked when this
  pedalboard already has it). **Install** downloads the `.openrig` file
  and runs it through the same import as a file (plan, store, TONE3000,
  new preset), with its progress and errors.
- **Link**: the account button shows a code and a QR code for
  `openrig.eu/link`. Approve it on the site, signed in, and the
  pedalboard shows **Signed in as @login**. The code expires after a few
  minutes; **Cancel** stops waiting. **Unlink** revokes the token on the
  site and deletes it here.
- When linked, **My favourites** lists your favourite rigs (tap one, then
  **Install**), and **Add to favourites** on a rig detail adds it (tap
  **Favourite** again to remove it). The detail asks the site whether
  the rig is a favourite (`favourited`); with a site that does not say,
  the marks come from the favourites list.
- **Publish preset** exports the current preset (MIDI CCs left out), asks
  for a style and a short description, and publishes it. If you already
  published a rig with that name (case and spaces aside, found in your
  own rigs on the site, `GET /me/rigs`, hidden ones included; or by a
  search kept to your rigs with a site that lacks it), it offers to
  publish a new version of it, with an optional changelog. The site's
  address of the rig is shown when done. The site takes rig files of
  512 KiB at most (`MAX_PUBLISH_BYTES`); a larger one is refused before
  upload. Items only this pedalboard has (`local` sources below)
  cannot be had by others, so they block publishing and are listed.

The account is kept in `<data>/openrig.json`, readable by its owner only
(mode 0600 on Linux and macOS), and the token is never logged. A 401 from
the site (token revoked from the site) unlinks the pedalboard.

The site is `https://openrig.eu`, or `$OPENRIG_RIGS_URL` (for a local
server, `http://` is accepted; any other address must be `https://`). An
account linked to one site is not used with another.

Every call runs on a worker thread, with a 10 s connection and a 30 s
overall timeout: without network the tab says so and the pedalboard keeps
playing. Browsing needs no account; favourites and publishing do. The
site's API is described in the community site repository
(`docs/API.md`).

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

Importing never runs anything from the file, whether it comes from a
drive or from openrig.eu. The rig is size-checked
(1 MiB at most, the format's `MAX_RIG_BYTES`, whether read from a drive or
downloaded; publishing is stricter, 512 KiB, see above) and validated by the crate before anything is installed.
Paths must be plain relative paths (no `..`, no absolute or drive paths,
no control characters). Downloads go where the app decides, not where the
rig says. Plugins come only from the signed store catalog, and models only
from TONE3000.

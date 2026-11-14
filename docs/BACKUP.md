# Backup and restore

A backup is one file that holds your presets, setlists, rigs, loops and
settings. Use it to move your sounds to a new SD card, to another OpenRig,
or to go back after a mistake.

## On the pedalboard

Settings → **Backup**.

- **Back up to USB drive** writes `openrig-backup-<date>-<time>.tar.gz` at
  the top of the first USB drive found (in its `OpenRig` folder on an ext4
  drive, see [OS.md](OS.md#usb-drives)). Backups are found in both places. With no drive, the backup stays on
  the pedal in `<data>/backups` (the last 3 are kept): download it from the
  remote control. The panel follows the drives plugged in and pulled out.
  **Eject**, under the button, makes that drive (and only it) safe to
  pull out (OpenRig
  OS, see [OS.md](OS.md#usb-drives)).
- **Include models and plugins** adds the NAM models, IRs and LV2 plugins.
  The file can then be large; the sizes are shown next to the switch.
  Without them, the backup lists the store plugins and TONE3000 models in
  use, and the pedal downloads them again after a restore. Models of your
  own and plugins copied by hand cannot be downloaded: the panel warns when
  you have some, and only a backup with models and plugins keeps them.
- **Restore** lists the backups found on USB drives and on the pedal. Tap
  one: a summary shows each part it replaces (with the number of files
  before and after) and each part it keeps. **Restore and restart**
  restarts the app, which swaps the data before it loads anything.
- After a restore, a message tells what is left to download. **Download**
  in the panel installs the store plugins (needs the network) and the
  TONE3000 models (needs a signed-in TONE3000 account).
- **Undo restore** brings back the data the last restore replaced. It is
  one step: undoing again brings the restore back.

## From the remote control

The Status page of the remote control (see [REMOTE.md](REMOTE.md)) has a
Backup section, for signed-in users only:

- **Download backup** saves the same file in the browser (tick **Include
  models and plugins** for the large files). `GET /api/backup`, or
  `/api/backup?full=1`.
- **Restore** uploads a backup file. The pedal checks it, then restarts to
  restore it. `POST /api/restore` with the file as the body.

The file is built in memory: 512 MiB at most each way. For larger backups
with models and plugins, use a USB drive.

## What is in a backup

| Part | Files in `<data>` | In a backup |
|---|---|---|
| Settings | `session.json`, `routing.json`, `hardware.json`, `network-midi.json`, `param-layout.json` | Always |
| Presets | `presets/` | Always |
| Setlists | `scenes/` | Always |
| Rigs | `rigs/` | Always |
| Loops | `loops/` | Always |
| Models | `models/` (NAM models, IRs, TONE3000 downloads) | With "Include models and plugins" |
| Plugins | `lv2/` (store and hand-copied LV2 bundles) | With "Include models and plugins" |

Never in a backup:

- Passwords and accounts: `remote.json` (remote control password),
  `tone3000.json` (TONE3000 sign-in), `openrig.json` (openrig.eu link).
  Sign in again after a restore on another pedal.
- Wi-Fi networks and their keys (kept by the OS, outside `<data>`).
- Caches: `store/` and `update/`.

A restore never touches these files either: they stay as they are.

## Archive format

A gzip-compressed tar file. Its first entry is `openrig-backup.json`, the
manifest:

| Field | What |
|---|---|
| `format` | Version of the layout (1). A newer one is refused |
| `app_version` | OpenRig version that wrote it |
| `created` | Date, RFC 3339 in UTC |
| `arch` | CPU of the pedal (`aarch64`, `x86_64`) |
| `parts` | Parts held (`settings`, `presets`, `setlists`, `rigs`, `loops`, `models`, `plugins`) |
| `files`, `bytes` | Every file, relative to `<data>`, and their total size |
| `store_plugins` | Store packages in use: package, version, bundle |
| `tone3000_models` | TONE3000 models in use: path, tone id, model id, name |
| `local_plugins`, `local_models` | What cannot be downloaded again |

The other entries are the files, with paths relative to `<data>`.

## Safety

Every archive is checked in full before anything changes:

- The manifest must come first, with a known format.
- Paths must be plain relative paths (no `..`, no absolute paths), inside
  a part the manifest names. Excluded files (passwords, accounts) are
  refused.
- Only regular files and folders: links and devices are refused.
- Every file must be in the manifest, once, and every file of the manifest
  must be in the archive.
- Caps: 200,000 files, 4 GiB per file, 64 GiB in total, 8 MiB of manifest.
- Plugins built for another CPU (a Pi 5 backup on an x86 pedal) are left
  out; the confirm screen says so.

The restore is atomic:

1. The archive is unpacked into `<data>/.restore/staging`, then renamed to
   `pending` once it is complete.
2. At the next start, before the session loads, each part to replace is
   moved to `<data>/.restore/undo.next`, then the restored part is moved
   in. Moves are renames in the same file system: no copy.
3. A power cut at any step is safe: the next start finishes the swap.
4. `undo.next` becomes `undo`, the one-step undo. It replaces the previous
   undo.

A damaged or refused archive changes nothing; the panel shows why.

## On a computer

The same panel works in the desktop app. `<data>` is
`$PEDALBOARD_DATA_DIR`, else `~/.local/share/pedalboard` on Linux.
USB drives are found as for rigs (see [RIGS.md](RIGS.md)).
`PEDALBOARD_MEDIA_DIR` names a folder whose subfolders are taken as USB
drives, to test without one.

After a restore the desktop app starts itself again. On the OS image the
launcher does it.

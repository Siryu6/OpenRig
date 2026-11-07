# Linux desktop packaging

This directory holds everything that turns a release build into
installable Linux packages. Built for the `Siryu6/OpenRig` release flow:
`.github/workflows/dist.yml` rebuilds all of these from the release tag
and attaches them to the GitHub release next to the `.tar.gz` files.

## Contents

| File | Purpose |
|---|---|
| `openrig.desktop` | desktop entry (`Exec=pedalboard`) |
| `openrig.svg` | application icon (pedal mark from `os/board/common/splash/splash.svg`) |
| `eu.openrig.OpenRig.metainfo.xml` | AppStream metadata for software centers |
| `build-deb.sh` | builds `openrig_<ver>_<arch>.deb` from `target/release/pedalboard` |
| `build-rpm.sh` | builds `openrig-<ver>-1.<arch>.rpm` (needs `rpmbuild`) |
| `build-appimage.sh` | builds `OpenRig-<ver>-<arch>.AppImage` (needs `appimagetool`) |
| `install.sh` | one-liner installer/uninstaller, see below |
| `aur/openrig-bin/` | AUR package: prebuilt release binary (maintainer: 9M2PJU) |
| `aur/openrig-git/` | AUR package: latest `main` from source (maintainer: 9M2PJU) |

Each build script takes `--bin PATH --version VER --arch ARCH --out DIR`
(defaults: `target/release/pedalboard`, version from `Cargo.toml`,
host arch, `dist/`). Every output gets a `.sha256` next to it, which
`install.sh` verifies before installing.

```sh
cargo build --release --locked
sh dist/build-deb.sh
sh dist/build-rpm.sh
APPIMAGETOOL=appimagetool sh dist/build-appimage.sh
```

Runtime dependencies (kept identical in deb/rpm/AUR/install notes):

- a JACK server: `jackd2` (or PipeWire's JACK: `pipewire-jack`)
- `liblilv` (LV2 hosting), `libasound2` (MIDI/ALSA), `libfontconfig1`,
  `libxkbcommon0` + `libxkbcommon-x11-0`, X11 (`libx11`, `libxcb`,
  `libxcursor`, `libxi`, `libxrender`), `libwayland-client` and GL
  (`libgl1` / `libglvnd`, EGL) for the UI — verified against the
  release binary's linked and runtime-loaded libraries.

## One-liner install / uninstall

```sh
# install the latest release
curl -fsSL https://raw.githubusercontent.com/Siryu6/OpenRig/main/dist/install.sh | sh
# pin a version (also: OPENRIG_VERSION=0.12.0, OPENRIG_REPO=Owner/Name)
curl -fsSL https://raw.githubusercontent.com/Siryu6/OpenRig/main/dist/install.sh | sh -s -- --version 0.12.0
# uninstall (works no matter which method installed it)
curl -fsSL https://raw.githubusercontent.com/Siryu6/OpenRig/main/dist/install.sh | sh -s -- --uninstall
```

Debian/Ubuntu/Mint/Pop!_OS/Raspberry Pi OS get the `.deb`,
Fedora/RHEL/Rocky/Alma the `.rpm` via `dnf`, openSUSE the `.rpm` via
`zypper`, Arch-likes get `openrig-bin` from the AUR (through `yay`/`paru`
when present), everything else gets the AppImage in `~/.local/bin` with a
desktop entry. Start JACK first, then run `pedalboard`.

## AUR

`openrig-bin` and `openrig-git` already exist on the AUR, maintained by
`robertfoster` and `awahab` respectively — not by this repo. This
directory keeps reviewed copies of their packaging so improvements go
through the normal pull-request flow here first:

- `install.sh` points Arch users at the existing `openrig-bin`
  (`yay -S openrig-bin`, or `openrig-git` for latest `main`).
- To propose a change (dependency fix, new release checksums, the
  desktop files in this directory), open a comment on the package's
  [AUR page](https://aur.archlinux.org/packages/openrig-bin) or mail
  the maintainer, with a link to the reviewed file here.
- `openrig-bin`'s tarball checksums change every release: fetch the new
  `.sha256` files from
  `.../releases/download/vX.Y.Z/openrig-X.Y.Z-linux-<arch>.tar.gz.sha256`,
  put them in `sha256sums_x86_64` / `sha256sums_aarch64`, bump `pkgver`,
  and regenerate `.SRCINFO` with `makepkg --printsrcinfo`.
- `openrig-git` needs no per-release bump: its `pkgver()` follows
  upstream `main` automatically.

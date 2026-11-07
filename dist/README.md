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

## AUR (maintainer notes for 9M2PJU)

The AUR packages live in their own AUR repos (`openrig-bin`,
`openrig-git`), not here. This directory keeps reviewed copies of the
`PKGBUILD` and `.SRCINFO` so updates go through the normal pull-request
flow. To publish:

```sh
# first time: create the AUR repos (needs an AUR account + ssh key)
git clone ssh://aur@aur.archlinux.org/openrig-bin.git
git clone ssh://aur@aur.archlinux.org/openrig-git.git

# on every upstream release X.Y.Z (example: openrig-bin)
cd openrig-bin
cp ../../OpenRig/dist/aur/openrig-bin/PKGBUILD .
cp ../../OpenRig/dist/openrig.desktop ../../OpenRig/dist/openrig.svg \
   ../../OpenRig/dist/eu.openrig.OpenRig.metainfo.xml .
# bump pkgver, replace the tarball sha256 with the new release's
# (.../releases/download/vX.Y.Z/openrig-X.Y.Z-linux-<arch>.tar.gz.sha256),
makepkg --printsrcinfo > .SRCINFO
makepkg -s   # test-build it
git add PKGBUILD .SRCINFO openrig.desktop openrig.svg eu.openrig.OpenRig.metainfo.xml
git commit -m "openrig-bin X.Y.Z" && git push
```

`openrig-git` needs no per-release bump: its `pkgver()` follows upstream
`main` automatically (update its `pkgver` placeholder only when the
`PKGBUILD` itself changes). After pushing either package, check its
[AUR page](https://aur.archlinux.org/packages/openrig-bin) builds.

# Pedalboard OS

A small Linux image that boots straight into the pedalboard: no desktop, no
login, the UI full screen on the display (DRM/KMS) and JACK on the audio
interface with a real-time kernel. Built with [Buildroot](https://buildroot.org)
from `os/`, for:

| Board | Image | Kernel |
|---|---|---|
| Raspberry Pi 5 (and Pi 500) | `os/output/rpi5/images/sdcard.img` | Raspberry Pi 6.12, `PREEMPT_RT` |
| x86_64 mini PC, UEFI (Intel graphics) | `os/output/x86_64/images/disk.img` | mainline 6.12, `PREEMPT_RT` |

Kernel fixes taken from newer Raspberry Pi kernels are in `os/board/rpi5/patches`
(why each one is there: its README).

## Build

On Linux, with Buildroot's
[host requirements](https://buildroot.org/downloads/manual/manual.html#requirement-mandatory)
(`build-essential`, `bc`, `cpio`, `rsync`, `unzip`, `wget`…):

```bash
git submodule update --init --recursive    # NeuralAudio
make -C os rpi5                             # or: make -C os x86_64
```

Besides the image, a build writes `update.raucb` next to it: the same
system as an update (see [Updates](#updates)).

The first run downloads Buildroot (pinned, checked by sha256) into
`os/.cache`, then every source into `os/.cache/dl`. A first build takes about
20 GB of disk and one to two hours (the x86_64 image also builds LLVM, for
the Intel graphics driver); later ones reuse what is built.

On macOS, Windows (WSL 2) or a Linux without the requirements, build in a
container (Docker, or Podman with `DOCKER=podman`; the checkout must be on
a case-sensitive file system, e.g. inside WSL rather than on `C:`):

```bash
make -C os docker-rpi5               # Podman: make -C os DOCKER=podman docker-rpi5
```

Other targets, for one board: `make -C os rpi5-menuconfig`,
`rpi5-linux-menuconfig`, `rpi5-pedalboard-rebuild rpi5` (rebuild the app,
then the image), `rpi5-clean`. `make -C os help` lists them.

The app is built with `--no-default-features --features
kms,lv2,nam,tone3000,midi,splash,update,store` (plus `gpio` on the Pi): no X11 /
Wayland. Its
dependencies are vendored from `Cargo.lock` at build time; the image's rustc
is 1.88, which `rust-version` in `Cargo.toml` keeps the lockfile compatible
with. Two dependencies are patched copies in `third_party/` (see their
`PATCHED.md`): lilv, which does not build on ARM Linux, and Slint's KMS
backend, which could not display on drivers without framebuffer modifiers
(virtio-gpu in QEMU).

### Test in QEMU

The x86_64 image (built first, `make -C os x86_64`) boots in QEMU, in a
container (Docker, or Podman with `DOCKER=podman`; KVM when `/dev/kvm`
exists):

```bash
make -C os qemu-x86_64 QEMU_KEYS=~/.ssh/id_ed25519.pub
ssh -p 2222 root@127.0.0.1       # 127.0.0.1, not localhost (IPv6)
```

The screen is on VNC `127.0.0.1:5900`. The test works on a copy of the
image in `os/output/x86_64/qemu`, with a root shell on the serial console
(`serial.sock`, logged to `serial.log`) and QMP on `qmp.sock`; delete
`disk.img` there to start again from a first boot (the keys are only copied
then). `QEMU_SCREEN=1024x600` sets
the display size. The copy's JACK buffer is
1024 x 3: QEMU's sound card cannot keep up with 128 x 2.

## Flash

Release images are on the [releases page](https://github.com/Siryu6/OpenRig/releases)
(`openrig-os-<version>-rpi5.img.xz`, `…-x86_64.img.xz`; the tools below
take them compressed). Write the image to a micro SD card (Pi) or a USB stick / SSD (mini PC) with
[Raspberry Pi Imager](https://www.raspberrypi.com/software/) ("Use custom"),
[balenaEtcher](https://etcher.balena.io), or `dd`:

```bash
sudo dd if=os/output/rpi5/images/sdcard.img of=/dev/sdX bs=4M conv=fsync status=progress
```

The mini PC boots it in UEFI mode; turn Secure Boot off.

## Settings

The first partition (FAT, readable from any computer) holds
`pedalboard.txt`:

| Key | Default | |
|---|---|---|
| `audio_device` | `auto` | First USB interface or sound HAT (HDMI is skipped), or a card name from `/proc/asound/cards`: `AudioBox`, `pisound`… |
| `sample_rate` | `48000` | Hz |
| `period`, `periods` | `128`, `2` | JACK buffer: 128 × 2 at 48 kHz adds about 5 ms each way |
| `display_rotation` | `0` | 0, 90, 180 or 270 |
| `wifi_country` | empty | Two letters (`FR`, `US`…): the Wi-Fi channels allowed there |

Values are read at boot; restart after editing. Keys are lowercase and
values plain (letters, digits, `_ . : , -`): anything else is ignored and the
default is used. Wi-Fi networks are chosen in
Settings → Wi-Fi, as on Raspberry Pi OS.

On the Pi, firmware settings of your own (a sound HAT's overlay, e.g.
`dtoverlay=pisound`) go in `extra.txt` on the boot partition in use (the
`SYSTEM` partition numbered `boot_partition` in `autoboot.txt`: 2 on a new
card), which `config.txt` includes. Updates keep it, and the kernel
arguments added to `cmdline.txt` there.

## Storage

Two copies of the system (slots A and B, for [updates](#updates)) and one
data partition:

| Raspberry Pi | x86_64 | Mounted | |
|---|---|---|---|
| 1, FAT `PEDALBOARD` | 1, FAT (EFI) `PEDALBOARD` | `/boot`, read-only | `pedalboard.txt`, `authorized_keys`, the slot to start (`autoboot.txt` / GRUB's `grubenv`) |
| 2, 3, FAT `SYSTEM` | | | Firmware and kernel of slots A and B |
| 4, 5, ext4 | 2, 3, ext4 | `/`, read-only | System of slots A and B (1 GB partitions, 512 MB file systems, the kernel in `/boot` on x86_64): never written while running, so a power cut cannot damage it |
| 6, ext4 | 4, ext4 | `/data` | Created on first boot over the rest of the disk |

System B and the data partition are created on first boot.

`/data` holds what changes: `/data/pedalboard` (presets, setlists, session,
NAM models, IRs, TONE3000 sign-in, LV2 bundles in `lv2/`), the Wi-Fi networks,
root's `authorized_keys` (`/data/ssh`), the SSH host keys, the update state
(`/data/update`, `/data/rauc`), app updates (`/data/app`) and `/var/lib`. It is checked at every boot. When it cannot
be mounted, the pedalboard runs on a RAM disk (logged): nothing is kept.

## Start-up and services

BusyBox init, no systemd. After mounting the storage, init starts and
restarts two services:

- **jackd**, on the audio interface, real-time priority 80. Without an
  interface it waits for one (plug it in at any time). The kernel's RCU
  threads run at priority 90 (`rcutree.kthread_prio=90`): an overloaded
  audio thread would otherwise starve them and hang the system (network,
  updates), not only crackle.
- **pedalboard**, the UI, full screen on the first display, as user
  `pedalboard`. When JACK stops or starts, the UI exits and is restarted with
  the audio, keeping the session. Menu ⋮ → Power off switches the board
  off (or restarts it) cleanly: the app saves the session and quits, then
  its launcher runs `poweroff` / `reboot` as root. An update being tried
  is kept first, and Restart also starts an installed update. On a Pi 5,
  its power button starts it again.

NetworkManager (Wi-Fi, Ethernet), chrony (time) and dropbear (SSH) run as
well, with what they need (udev, syslog, D-Bus); nothing else.

A boot logo shows from early in the boot until the UI appears:
`/usr/libexec/pedalboard/splash` (the `pedalboard-splash` binary), started
by `/etc/init.d/S01splash`, draws
`/usr/share/pedalboard/splash.png` centered on the app background, turned
like `display_rotation`, and scaled down if it does not fit. On a Pi 5 the
firmware's framebuffer comes first (under a second after power-on), then the
display driver replaces it about half a second later: the splash watches
the framebuffer for 10 s and draws the logo again on each new one. The logo
comes from `os/board/common/splash/splash.svg`, rendered by hand (command
in the SVG); the build does not render it again. Any PNG works. Before Linux drives the display, the screen may show its own
"No signal" message.

## SSH and logs

Put your public keys in a file named `authorized_keys` on the FAT partition
(copied at each boot; removing the file revokes them), then connect to the
address shown in Settings → Wi-Fi (Ethernet included):

```bash
ssh root@192.168.1.42         # the address your router gave it
tail -f /var/log/messages     # jackd, pedalboard, storage messages
cat /etc/pedalboard-release   # version of the image
pedalboard-report > /tmp/report.txt   # everything a bug report needs
```

`pedalboard-report` gathers the versions, settings, sound cards and how
JACK opened them, the HAT and `extra.txt` (Pi), the kernel's audio
messages and the log of jackd and the app. Attach its output to an issue.

`/var/log` is in RAM: the logs start again at each boot.

There is no password login. Files copied to `/data/pedalboard` must belong
to the user: `chown -R pedalboard:pedalboard /data/pedalboard`.

## Troubleshooting

- **Black screen, nothing on SSH.** Edit `cmdline.txt` from a computer
  (Pi: on the `SYSTEM` partition in use, 2 on a new card; x86_64:
  `EFI/BOOT/grub.cfg`): remove `quiet loglevel=3` and add `console=tty1`
  to see the boot messages on the display. Keep it on one line.
- **Wrong resolution, or no picture from an HDMI screen powered by the Pi.**
  Some small panels send no or wrong display data (EDID), or are not ready
  when the Pi starts. Add the mode at the end of `cmdline.txt`, e.g.
  `video=HDMI-A-1:1024x600@60D` for a 10" 1024 x 600 screen (`D` forces
  the output on).
- **USB devices dropping out (screen touch, audio interface).** The Pi 5
  gives USB devices 1.6 A only with the official 27 W (5 V 5 A) supply,
  600 mA otherwise: a screen and an audio interface both powered by the Pi
  need it.
- **No sound.** `cat /proc/asound/cards` lists the interfaces the kernel
  sees; set `audio_device` in `pedalboard.txt` to one of the names in
  brackets. `tail -f /var/log/messages` shows why jackd does not start.
- **An output stays silent while its meter moves** (multi-channel
  interfaces and HATs). At start the app logs the interface ports it sees
  and what it connected (`grep -E "Interface|Connected|connect" /var/log/messages`):
  if `pedalboard:out_3 → system:playback_3` is there, the signal leaves
  OpenRig and the silence is past JACK. `cat /proc/asound/card<card>/pcm0p/sub0/hw_params`
  shows how many channels JACK opened the card with. Many multi-channel
  cards and HATs have mixer controls per channel that start muted or at
  zero: check them with `amixer -c <card> contents` (card number from
  `cat /proc/asound/cards`). To test the card without OpenRig, free it
  from jackd and play a test on every channel, in one line:
  `killall jackd; speaker-test -D hw:<card> -c 8 -t wav`. Init starts
  jackd again 2 s later; it waits for the card until the test ends
  (Ctrl+C).

## Updates

Settings → Updates → **Check for updates** compares the app with the
latest [release](https://github.com/Siryu6/OpenRig/releases) and offers
what to install, over Wi-Fi or Ethernet:

- **App update**: when the release was built on the same system as the
  one running, only the app is replaced (a few MB, the app restarts in a
  few seconds).
- **System update**: when the release needs a new system (kernel,
  libraries, drivers, scripts…), the whole system is installed on the
  other slot and the pedalboard restarts on it.

Both keep `/data` (presets, setlists, models, Wi-Fi networks) and the first
partition (`pedalboard.txt`, `authorized_keys`). Nothing is checked or
downloaded without the button.

### App or system: `OS_ID`

Each image records in `/etc/pedalboard-release` its version, the app's
(`APP_VERSION`), its board and `OS_ID`: a fingerprint of everything in the
system but the app (Buildroot's version, the board's defconfig, and every
file of `os/board`, `os/package` but `package/pedalboard/pedalboard.mk`,
and the external tree; computed by `board/common/post-build.sh`). Two
releases with the same `OS_ID` have the same system, so the app alone can
go from one to the other. A change in the system, including a library the
app's package selects (`package/pedalboard/Config.in`), changes the
`OS_ID` by itself: the next release then requires a system update, with
nothing to remember.

Each build makes two signed bundles in `images/` (`board/common/make-bundle.sh`):
`update.raucb`, the whole system, and `app.raucb`, the app alone with the
`OS_ID` it was built for. A release publishes per board
(`os/release-assets.sh`):

| File | |
|---|---|
| `openrig-os-<version>-<board>.img.xz` | Image to [flash](#flash) |
| `openrig-os-<version>-<board>.raucb` | System update |
| `openrig-os-<version>-<board>-app.raucb` | App update |
| `openrig-os-<version>-<board>.json` | What the pedalboard reads: version, board, `OS_ID`, and the two bundles (file, size, SHA-256) |
| `openrig-os-<version>-<board>.sha256` | Checksums |

The pedalboard picks the app update when the `.json`'s `OS_ID` is its own,
else the system update. Until the `.json` of its board is published (the
release images take hours to build), it reports the release as being
prepared.

### How it installs

The app runs as the `pedalboard` user and cannot install anything: it
downloads the bundle into `/data/pedalboard/update`, checks its SHA-256,
then asks `/usr/libexec/pedalboard/updater` (started by init, as root)
through the FIFO `/run/pedalboard/update/request`. The updater copies the
bundle out of the app's reach and runs `pedalboard-update`, which checks
its signature with RAUC like any update:

- a **system update** (`pedalboard-update install`) is written to the
  other slot and tried at the next restart (below);
- an **app update** (`pedalboard-update install-app`) is extracted to
  `/data/app/<version>`, refused if its `OS_ID` is not the system's, and
  tried at the next start of the app: the UI launcher
  (`/usr/libexec/pedalboard/ui`) runs it instead of `/usr/bin/pedalboard`.

The app tried is kept once it has run 30 seconds and still responds (it
writes `/run/pedalboard/app/alive` again every second; the launcher removes
it at 25 s and looks for it at 30 s), or when it is switched off or
restarted from its menu before. It is undone, and the previous app runs
again, when it crashes, hangs (the launcher stops it at 30 s), or the power
is cut before it was kept. When it quits by itself before (the JACK server
stopped or started), it is tried again, up to three times.

An app update runs only on the system it was installed on. After a system
update, the system's own app runs (it is at least as new); the app update
is used again if the board goes back to the previous system, and deleted
once the new one is kept.

### By hand

The same bundles install over SSH, e.g. from a release or a local build:

```bash
scp -O os/output/rpi5/images/update.raucb root@192.168.1.42:/data/
ssh root@192.168.1.42
pedalboard-update install /data/update.raucb   # written to the slot not running
pedalboard-update reboot                       # start it, once
pedalboard-update status                       # slots, versions, last update
rm /data/update.raucb
```

and `pedalboard-update install-app /data/app.raucb` for an app update,
used at the next start of the app. A system installed by hand replaces any
app update, like one installed from Settings.

### Trial and rollback

A system update is written to the other slot while the pedalboard keeps
playing (about a minute), then tried once at the next reboot. If the
pedalboard UI keeps running for 30 seconds on it, it becomes the default.
If it does not (the system does not start, crashes, or the UI does not
come up within four minutes), the board restarts on the previous system
and `pedalboard-update status` reports the update failed. A power cut
during the install or the trial start leaves the previous system in place.
Switching off or restarting from the menu during the trial keeps the
update: the UI was running. The one-time start uses the firmware's tryboot flag
on the Pi (set by `rpi-tryboot`, an image package) and GRUB's `grubenv` on
x86_64.

### Signing key

Updates are checked with [RAUC](https://rauc.io): the board installs only
bundles signed with the key of the build that made its image. That key is
created on the first build in `os/keys` (or `PEDALBOARD_UPDATE_KEYS`):
keep it, outside of git. Images built with another key refuse its
updates; to accept several keys, put their certificates together in
`/etc/rauc/keyring.pem` at build time. `PEDALBOARD_OS_VERSION` sets the
version shown (default: `git describe`).

The release images are built by GitHub Actions
(`.github/workflows/build-release.yml`) with the key and certificate of the
repository secrets `OS_UPDATE_KEY` and `OS_UPDATE_CERT` (the contents of
`os/keys/key.pem` and `cert.pem`); without them the build stops rather
than sign with a new key. An image built locally with another key only
takes updates from GitHub after flashing a release image once.

What an update does not change: the partitions, the Raspberry Pi
EEPROM bootloader, and on x86_64 GRUB and `grub.cfg` (on the EFI
partition). The Pi's boot partition, `config.txt` included, is part of
the slot.

## LV2 plugins

The image ships no LV2 plugins: the NAM, cabinet (IR) and built-in blocks
work out of the box. The Store page installs plugins of the OpenRig catalog
into `/data/pedalboard/lv2` (see [STORE.md](STORE.md)). Bundles copied there
by hand are found at the next start, or from the Store's refresh button. They must be built for the board (aarch64 on the Pi, x86_64
on the mini PC) against a glibc no newer than the image's; the simplest way
is to add their Buildroot package to the defconfig.

## Without the image

The app also runs on Raspberry Pi OS or any Linux: build it as in the README
and start JACK, then `pedalboard` from the desktop. For a full-screen start
without a desktop, build with `--features kms` and run it from a console
with `SLINT_BACKEND=linuxkms`; for real-time audio, use a `PREEMPT_RT`
kernel and give the user real-time limits (`@audio - rtprio 95`,
`@audio - memlock unlimited` in `/etc/security/limits.d/audio.conf`).

## Not done yet

- The x86_64 image is tested in QEMU only. The Pi 5 image runs on a Pi 5
  (8 GB) with a Scarlett 2i2 at 128 x 2, HDMI display, Wi-Fi and Ethernet,
  A/B updates and their rollback.
- Updates from the pedalboard itself (Settings: download over Wi-Fi or
  from a USB stick, then restart): for now with `pedalboard-update` over
  SSH. Writing a whole image to the card replaces the partition table, so
  `/data` is recreated empty: back up `/data/pedalboard` first.
- AMD graphics on x86_64 (needs the amdgpu firmware and driver), Wi-Fi
  hotspot for first set-up, `.local` names
  (mDNS).

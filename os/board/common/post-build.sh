#!/bin/sh
#
# Common post-build step of the pedalboard OS images

set -eu

TARGET=$1
OS_DIR=$(cd "$(dirname "$0")/../.." && pwd)

# Mount points and bind-mount targets on the read-only root (see S00storage)
mkdir -p "$TARGET/boot" "$TARGET/data" "$TARGET/media" "$TARGET/root/.ssh" \
	"$TARGET/etc/NetworkManager/system-connections"

# dnsmasq is for the remote control's hotspot, started by NetworkManager
# (DHCP and DNS on 10.42.0.1), not as a system service
rm -f "$TARGET/etc/init.d/S80dnsmasq"

# avahi-daemon only runs with Network MIDI, started by
# /usr/libexec/pedalboard/network-midi
rm -f "$TARGET/etc/init.d/S50avahi-daemon"

# Updates are accepted when signed with this build's key (update-keys.sh)
# shellcheck source=update-keys.sh
. "$OS_DIR/board/common/update-keys.sh"
install -D -m 0644 "$UPDATE_CERT" "$TARGET/etc/rauc/keyring.pem"

# Image version, shown by `cat /etc/pedalboard-release`
version=${PEDALBOARD_OS_VERSION:-$(git -C "$OS_DIR/.." describe --always --dirty 2>/dev/null || echo unknown)}
app_version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$OS_DIR/../Cargo.toml" | head -1)
defconfig=$(sed -n 's/^BR2_DEFCONFIG="\(.*\)"/\1/p' "$BR2_CONFIG")
board=$(basename "$defconfig" _defconfig)
board=${board#pedalboard_}

# Kconfig drops an option of the defconfig with only a warning when its
# dependencies cannot be met (a recursive dependency left rtpmidid out of
# v0.13.0): fail the build when a package the pedalboard runs is missing.
# One "option file" per line.
while read -r option file; do
	grep -qx "$option=y" "$defconfig" || continue
	if ! grep -qx "$option=y" "$BR2_CONFIG" || [ ! -e "$TARGET$file" ]; then
		echo "post-build: $option is in $(basename "$defconfig") but $file is not in the image" >&2
		exit 1
	fi
done <<PACKAGES
BR2_PACKAGE_RTPMIDID /usr/bin/rtpmidid
PACKAGES

# Fingerprint of the system without the app: Buildroot's version, the
# board's configuration and the pedalboard tree (scripts, overlays, kernel
# fragments, packages, the libraries the app's package selects) except the
# app's build recipe. An app built for one system runs on every system with
# the same OS_ID, so an update between them only replaces the app
# (docs/OS.md, "Updates").
os_id=$(
	cd "$OS_DIR"
	{
		grep -E '^BR_(VERSION|SHA256) ' Makefile
		sha256sum "configs/$(basename "$defconfig")"
		find board package Config.in external.desc external.mk -type f \
			! -path 'package/pedalboard/pedalboard.mk' | LC_ALL=C sort | xargs sha256sum
	} | sha256sum | cut -c1-16
)

cat >"$TARGET/etc/pedalboard-release" <<RELEASE
NAME="Pedalboard OS"
VERSION="$version"
APP_VERSION="$app_version"
BOARD="$board"
OS_ID="$os_id"
BUILD_DATE="$(date -u +%Y-%m-%d)"
RELEASE

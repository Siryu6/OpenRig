#!/bin/sh
#
# Common post-build step of the pedalboard OS images

set -eu

TARGET=$1
OS_DIR=$(cd "$(dirname "$0")/../.." && pwd)

# Mount points and bind-mount targets on the read-only root (see S00storage)
mkdir -p "$TARGET/boot" "$TARGET/data" "$TARGET/root/.ssh" \
	"$TARGET/etc/NetworkManager/system-connections"

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

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
. "$OS_DIR/board/common/update-keys.sh"
install -D -m 0644 "$UPDATE_CERT" "$TARGET/etc/rauc/keyring.pem"

# Image version, shown by `cat /etc/pedalboard-release`
version=${PEDALBOARD_OS_VERSION:-$(git -C "$OS_DIR/.." describe --always --dirty 2>/dev/null || echo unknown)}
cat >"$TARGET/etc/pedalboard-release" <<RELEASE
NAME="Pedalboard OS"
VERSION="$version"
BUILD_DATE="$(date -u +%Y-%m-%d)"
RELEASE

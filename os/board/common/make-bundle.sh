#!/bin/sh
#
# Update bundles of the images just built (RAUC, signed with the update key,
# see post-build.sh), from a post-image script:
#
#   make-bundle.sh <compatible> <image>=<slot class>...
#
# update.raucb holds the whole system (the images given), app.raucb only the
# app: pedalboard-update installs it on the data partition, for a system
# with the same OS_ID (/etc/pedalboard-release, docs/OS.md "Updates").

set -eu

COMMON_DIR=$(cd "$(dirname "$0")" && pwd)
. "$COMMON_DIR/update-keys.sh"
compatible=$1
shift

release() {
	sed -n "s/^$1=\"\\(.*\\)\"/\\1/p" "$TARGET_DIR/etc/pedalboard-release"
}
version=$(release VERSION)
work="$BUILD_DIR/bundle.tmp"
rm -rf "$work"
mkdir -p "$work"
{
	printf '[update]\ncompatible=%s\nversion=%s\n\n[bundle]\nformat=verity\n' \
		"$compatible" "$version"
	for spec in "$@"; do
		file=${spec%%=*}
		cp "$BINARIES_DIR/$file" "$work/$file"
		printf '\n[image.%s]\nfilename=%s\n' "${spec#*=}" "$file"
	done
} >"$work/manifest.raucm"

bundle="$BINARIES_DIR/update.raucb"
rm -f "$bundle"
"$HOST_DIR/bin/rauc" bundle --cert="$UPDATE_CERT" --key="$UPDATE_KEY" \
	--mksquashfs-args="-comp xz" "$work" "$bundle"
rm -rf "$work"
echo "Update bundle: $bundle ($version)"

# The app alone: its binary and the system it was built for. The slot class
# "app" matches no slot, so `rauc install` refuses this bundle.
mkdir -p "$work/app"
cp "$TARGET_DIR/usr/bin/pedalboard" "$work/app/pedalboard"
release APP_VERSION >"$work/app/version"
release OS_ID >"$work/app/os-id"
tar -C "$work/app" --owner=0 --group=0 -cf "$work/app.tar" pedalboard version os-id
rm -rf "$work/app"
printf '[update]\ncompatible=%s\nversion=%s\n\n[bundle]\nformat=verity\n\n[image.app]\nfilename=app.tar\n' \
	"$compatible" "$(release APP_VERSION)" >"$work/manifest.raucm"
bundle="$BINARIES_DIR/app.raucb"
rm -f "$bundle"
"$HOST_DIR/bin/rauc" bundle --cert="$UPDATE_CERT" --key="$UPDATE_KEY" \
	--mksquashfs-args="-comp xz" "$work" "$bundle"
rm -rf "$work"
echo "App update bundle: $bundle ($(release APP_VERSION))"

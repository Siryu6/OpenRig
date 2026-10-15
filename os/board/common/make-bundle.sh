#!/bin/sh
#
# Update bundle of the images just built (RAUC, signed with the update key,
# see post-build.sh), from a post-image script:
#
#   make-bundle.sh <compatible> <image>=<slot class>...

set -eu

COMMON_DIR=$(cd "$(dirname "$0")" && pwd)
. "$COMMON_DIR/update-keys.sh"
compatible=$1
shift

version=$(sed -n 's/^VERSION="\(.*\)"/\1/p' "$TARGET_DIR/etc/pedalboard-release")
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

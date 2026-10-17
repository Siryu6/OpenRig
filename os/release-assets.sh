#!/bin/sh
#
# Release files of a board built with `make -C os <board>`, as GitHub
# releases publish them (.github/workflows/build-release.yml):
#
#   os/release-assets.sh <board> <version> <output directory>
#
#   openrig-os-<version>-<board>.img.xz       image to write to a card / disk
#   openrig-os-<version>-<board>.raucb        update of the whole system
#   openrig-os-<version>-<board>-app.raucb    update of the app alone
#   openrig-os-<version>-<board>.json         what "Check for updates" reads:
#                                             the system's OS_ID and both
#                                             bundles (file, size, sha256)
#   openrig-os-<version>-<board>.sha256       checksums of the files above
#
# Publish the .json last: a pedalboard reading it expects the bundles.
# See docs/OS.md, "Updates".

set -eu

[ $# -eq 3 ] || {
	sed -n '6s/^# \{0,1\}//p' "$0" >&2
	exit 2
}
board=$1
version=$2
out=$3
OS_DIR=$(cd "$(dirname "$0")" && pwd)
images=$OS_DIR/output/$board/images
release=$OS_DIR/output/$board/target/etc/pedalboard-release
name=openrig-os-$version-$board

case $board in
rpi5) image=$images/sdcard.img ;;
*) image=$images/disk.img ;;
esac
os_id=$(sed -n 's/^OS_ID="\(.*\)"/\1/p' "$release")
[ -n "$os_id" ] || {
	echo "No OS_ID in $release" >&2
	exit 1
}

mkdir -p "$out"
cd "$out"
xz -T0 -6 -c "$image" >"$name.img.xz"
cp "$images/update.raucb" "$name.raucb"
cp "$images/app.raucb" "$name-app.raucb"

bundle() {
	printf '{"file":"%s","size":%s,"sha256":"%s"}' \
		"$1" "$(wc -c <"$1" | tr -d ' ')" "$(sha256sum "$1" | cut -d' ' -f1)"
}
printf '{"version":"%s","board":"%s","os_id":"%s","system":%s,"app":%s}\n' \
	"$version" "$board" "$os_id" "$(bundle "$name.raucb")" "$(bundle "$name-app.raucb")" \
	>"$name.json"

sha256sum "$name.img.xz" "$name.raucb" "$name-app.raucb" "$name.json" >"$name.sha256"
ls -l "$name"*

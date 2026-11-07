#!/bin/sh
# Build the OpenRig .deb from a release binary.
#
# Usage: dist/build-deb.sh [--bin PATH] [--version VER] [--arch ARCH] [--out DIR]
#
# Defaults: --bin target/release/pedalboard, --version from Cargo.toml,
# --arch from dpkg (falls back to uname -m), --out dist/.
# Output: dist/openrig_<version>_<debarch>.deb and its .sha256.

set -eu

REPO=$(cd "$(dirname "$0")/.." && pwd)
BIN="$REPO/target/release/pedalboard"
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' "$REPO/Cargo.toml" | head -n 1)
ARCH=$(dpkg --print-architecture 2>/dev/null || uname -m)
OUT="$REPO/dist"

while [ $# -gt 0 ]; do
	case $1 in
	--bin) BIN=$2; shift 2 ;;
	--version) VERSION=$2; shift 2 ;;
	--arch) ARCH=$2; shift 2 ;;
	--out) OUT=$2; shift 2 ;;
	*) echo "Unknown option: $1" >&2; exit 2 ;;
	esac
done

case $ARCH in
x86_64 | amd64) DEBARCH=amd64 ;;
aarch64 | arm64) DEBARCH=arm64 ;;
*) echo "Unsupported architecture: $ARCH (want x86_64 or aarch64)" >&2; exit 1 ;;
esac

[ -x "$BIN" ] || {
	echo "Binary not found or not executable: $BIN (build with: cargo build --release --locked)" >&2
	exit 1
}

STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT INT TERM
PKGDIR=$STAGE/openrig_${VERSION}_${DEBARCH}

mkdir -p "$PKGDIR/DEBIAN" \
	"$PKGDIR/usr/bin" \
	"$PKGDIR/usr/share/applications" \
	"$PKGDIR/usr/share/icons/hicolor/scalable/apps" \
	"$PKGDIR/usr/share/metainfo" \
	"$PKGDIR/usr/share/doc/openrig"

cat >"$PKGDIR/DEBIAN/control" <<EOF
Package: openrig
Version: $VERSION
Architecture: $DEBARCH
Maintainer: Siryu6 <Siryu6@users.noreply.github.com>
Section: sound
Priority: optional
Depends: jackd2 | pipewire-jack, liblilv-0-0 | liblilv-0-3t64, libasound2 | libasound2t64, libfontconfig1, libxkbcommon0
Homepage: https://openrig.eu
Description: Open source guitar pedalboard
 Turns a small computer into a multi-effects pedalboard: neural amp
 captures, impulse responses, LV2 plugins, a plugin store, a TONE3000
 library, live scenes, tuner, looper and MIDI.
 .
 Needs JACK running (jackd2, or PipeWire's JACK) before starting.
EOF

install -m 0755 "$BIN" "$PKGDIR/usr/bin/pedalboard"
install -m 0644 "$REPO/dist/openrig.desktop" "$PKGDIR/usr/share/applications/openrig.desktop"
install -m 0644 "$REPO/dist/openrig.svg" "$PKGDIR/usr/share/icons/hicolor/scalable/apps/openrig.svg"
install -m 0644 "$REPO/dist/eu.openrig.OpenRig.metainfo.xml" "$PKGDIR/usr/share/metainfo/eu.openrig.OpenRig.metainfo.xml"
install -m 0644 "$REPO/LICENSE" "$PKGDIR/usr/share/doc/openrig/copyright"

mkdir -p "$OUT"
dpkg-deb --root-owner-group --build "$PKGDIR" "$OUT/openrig_${VERSION}_${DEBARCH}.deb" >/dev/null
(cd "$OUT" && sha256sum "openrig_${VERSION}_${DEBARCH}.deb" >"openrig_${VERSION}_${DEBARCH}.deb.sha256")
echo "Wrote $OUT/openrig_${VERSION}_${DEBARCH}.deb"

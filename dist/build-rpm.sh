#!/bin/sh
# Build the OpenRig .rpm from a release binary.
#
# Usage: dist/build-rpm.sh [--bin PATH] [--version VER] [--arch ARCH] [--out DIR]
#
# Defaults: --bin target/release/pedalboard, --version from Cargo.toml,
# --arch from uname -m, --out dist/.
# Output: dist/openrig-<version>-1.<rpmarch>.rpm and its .sha256.
# Needs: rpmbuild.

set -eu

REPO=$(cd "$(dirname "$0")/.." && pwd)
BIN="$REPO/target/release/pedalboard"
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' "$REPO/Cargo.toml" | head -n 1)
ARCH=$(uname -m)
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
x86_64) RPMARCH=x86_64 ;;
aarch64) RPMARCH=aarch64 ;;
*) echo "Unsupported architecture: $ARCH (want x86_64 or aarch64)" >&2; exit 1 ;;
esac

[ -x "$BIN" ] || {
	echo "Binary not found or not executable: $BIN (build with: cargo build --release --locked)" >&2
	exit 1
}
command -v rpmbuild >/dev/null || {
	echo "rpmbuild not found (Debian/Ubuntu: sudo apt install rpm)" >&2
	exit 1
}

TOP=$(mktemp -d)
trap 'rm -rf "$TOP"' EXIT INT TERM
mkdir -p "$TOP/BUILD" "$TOP/RPMS" "$TOP/SOURCES" "$TOP/SPECS" "$TOP/SRPMS" \
	"$TOP/BUILDROOT/openrig-$VERSION-1.$RPMARCH"

ROOT=$TOP/BUILDROOT/openrig-$VERSION-1.$RPMARCH
mkdir -p "$ROOT/usr/bin" \
	"$ROOT/usr/share/applications" \
	"$ROOT/usr/share/icons/hicolor/scalable/apps" \
	"$ROOT/usr/share/metainfo" \
	"$ROOT/usr/share/licenses/openrig"

install -m 0755 "$BIN" "$ROOT/usr/bin/pedalboard"
install -m 0644 "$REPO/dist/openrig.desktop" "$ROOT/usr/share/applications/openrig.desktop"
install -m 0644 "$REPO/dist/openrig.svg" "$ROOT/usr/share/icons/hicolor/scalable/apps/openrig.svg"
install -m 0644 "$REPO/dist/eu.openrig.OpenRig.metainfo.xml" "$ROOT/usr/share/metainfo/eu.openrig.OpenRig.metainfo.xml"
install -m 0644 "$REPO/LICENSE" "$ROOT/usr/share/licenses/openrig/LICENSE"

cat >"$TOP/SPECS/openrig.spec" <<EOF
Name: openrig
Version: $VERSION
Release: 1%{?dist}
Summary: Open source guitar pedalboard
License: GPL-3.0-or-later
URL: https://openrig.eu
BuildArch: $RPMARCH
Requires: (jack-audio-connection-kit or pipewire-jack-audio-connection-kit)
Requires: lilv
Requires: alsa-lib
Requires: fontconfig
Requires: libxkbcommon

%description
Turns a small computer into a multi-effects pedalboard: neural amp
captures, impulse responses, LV2 plugins, a plugin store, a TONE3000
library, live scenes, tuner, looper and MIDI. Needs JACK running
before starting.

%install
cp -a "$ROOT"/* "%{buildroot}/"

%files
/usr/bin/pedalboard
/usr/share/applications/openrig.desktop
/usr/share/icons/hicolor/scalable/apps/openrig.svg
/usr/share/metainfo/eu.openrig.OpenRig.metainfo.xml
/usr/share/licenses/openrig/LICENSE
EOF

rpmbuild -bb --define "_topdir $TOP" --target "$RPMARCH" "$TOP/SPECS/openrig.spec" >/dev/null
mkdir -p "$OUT"
cp "$TOP/RPMS/$RPMARCH/openrig-$VERSION-1.$RPMARCH.rpm" "$OUT/"
(cd "$OUT" && sha256sum "openrig-$VERSION-1.$RPMARCH.rpm" >"openrig-$VERSION-1.$RPMARCH.rpm.sha256")
echo "Wrote $OUT/openrig-$VERSION-1.$RPMARCH.rpm"

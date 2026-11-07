#!/bin/sh
# Build the OpenRig AppImage from a release binary.
#
# Usage: dist/build-appimage.sh [--bin PATH] [--version VER] [--arch ARCH] [--out DIR]
#
# Defaults: --bin target/release/pedalboard, --version from Cargo.toml,
# --arch from uname -m, --out dist/.
# Output: dist/OpenRig-<version>-<arch>.AppImage and its .sha256.
# Needs: appimagetool in PATH, or APPIMAGETOOL=/path/to/appimagetool.
#
# The AppImage ships the app itself; JACK and lilv still come from the host
# (e.g. `apt install jackd2 liblilv-0-0`), like the .tar.gz in the release.

set -eu

REPO=$(cd "$(dirname "$0")/.." && pwd)
BIN="$REPO/target/release/pedalboard"
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' "$REPO/Cargo.toml" | head -n 1)
ARCH=$(uname -m)
OUT="$REPO/dist"
APPIMAGETOOL=${APPIMAGETOOL:-appimagetool}

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
x86_64 | aarch64) ;;
*) echo "Unsupported architecture: $ARCH (want x86_64 or aarch64)" >&2; exit 1 ;;
esac

[ -x "$BIN" ] || {
	echo "Binary not found or not executable: $BIN (build with: cargo build --release --locked)" >&2
	exit 1
}
command -v "$APPIMAGETOOL" >/dev/null || {
	echo "appimagetool not found: set APPIMAGETOOL or put it in PATH (https://github.com/AppImage/appimagetool)" >&2
	exit 1
}

APPDIR=$(mktemp -d)
trap 'rm -rf "$APPDIR"' EXIT INT TERM
mkdir -p "$APPDIR/usr/bin"

install -m 0755 "$BIN" "$APPDIR/usr/bin/pedalboard"
install -m 0644 "$REPO/dist/openrig.desktop" "$APPDIR/openrig.desktop"
install -m 0644 "$REPO/dist/openrig.svg" "$APPDIR/openrig.svg"
# appimagetool builds .DirIcon from a top-level PNG matching Icon=openrig
install -m 0644 "$REPO/dist/openrig.png" "$APPDIR/openrig.png"
chmod 0755 "$APPDIR"

cat >"$APPDIR/AppRun" <<'EOF'
#!/bin/sh
HERE=$(dirname "$(readlink -f "$0")")
exec "$HERE/usr/bin/pedalboard" "$@"
EOF
chmod 0755 "$APPDIR/AppRun"

mkdir -p "$OUT"
ARCH=$ARCH "$APPIMAGETOOL" "$APPDIR" "$OUT/OpenRig-$VERSION-$ARCH.AppImage" >/dev/null
(cd "$OUT" && sha256sum "OpenRig-$VERSION-$ARCH.AppImage" >"OpenRig-$VERSION-$ARCH.AppImage.sha256")
echo "Wrote $OUT/OpenRig-$VERSION-$ARCH.AppImage"

#!/bin/sh
# OpenRig one-liner installer for Linux desktops.
#
# Install latest release:
#   curl -fsSL https://raw.githubusercontent.com/Siryu6/OpenRig/main/dist/install.sh | sh
# Install a pinned version:
#   curl -fsSL https://raw.githubusercontent.com/Siryu6/OpenRig/main/dist/install.sh | sh -s -- --version 0.12.0
# Uninstall:
#   curl -fsSL https://raw.githubusercontent.com/Siryu6/OpenRig/main/dist/install.sh | sh -s -- --uninstall
#
# What it does, by distribution:
#   Debian / Ubuntu / Mint / Pop!_OS / Raspberry Pi OS: installs the .deb
#   Fedora / RHEL / Rocky / Alma: installs the .rpm with dnf
#   openSUSE: installs the .rpm with zypper
#   Arch / Manjaro / EndeavourOS / CachyOS: installs openrig-bin from the AUR
#     (with yay or paru when present), else falls back to the AppImage
#   anything else: installs the AppImage to ~/.local/bin with a desktop entry
#
# Environment overrides: OPENRIG_REPO (default Siryu6/OpenRig),
# OPENRIG_VERSION (default: latest release), OPENRIG_NO_SUDO (any value
# skips sudo and installs the AppImage/tarball into ~/.local/bin instead).

set -eu

REPO=${OPENRIG_REPO:-Siryu6/OpenRig}
WANT_VERSION=${OPENRIG_VERSION:-}
UNINSTALL=0

while [ $# -gt 0 ]; do
	case $1 in
	--uninstall) UNINSTALL=1; shift ;;
	--version) WANT_VERSION=$2; shift 2 ;;
	--repo) REPO=$2; shift 2 ;;
	-h | --help)
		echo "Usage: install.sh [--version VER] [--repo OWNER/NAME] [--uninstall]"
		exit 0
		;;
	*) echo "Unknown option: $1" >&2; exit 2 ;;
	esac
done

need() {
	command -v "$1" >/dev/null || {
		echo "Need '$1' to install OpenRig" >&2
		exit 1
	}
}

# down URL FILE: fetch URL to FILE (curl or wget, whichever exists)
down() {
	if command -v curl >/dev/null; then
		curl -fsSL -o "$2" "$1"
	elif command -v wget >/dev/null; then
		wget -q -O "$2" "$1"
	else
		echo "Need curl or wget to install OpenRig" >&2
		exit 1
	fi
}

# latest_tag: newest release tag (vX.Y.Z) of $REPO
latest_tag() {
	down "https://api.github.com/repos/$REPO/releases/latest" - |
		sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\(v[0-9][^"]*\)".*/\1/p' |
		head -n 1
}

sudo_if_needed() {
	if [ "$(id -u)" -eq 0 ] || [ -n "${OPENRIG_NO_SUDO:-}" ]; then
		"$@"
	elif command -v sudo >/dev/null; then
		sudo "$@"
	else
		echo "Need root or sudo to install system packages" >&2
		exit 1
	fi
}

uninstall_everywhere() {
	echo "Removing OpenRig..."
	# Native packages (ignore failures: it may have been another method)
	if command -v apt-get >/dev/null; then sudo_if_needed apt-get remove -y openrig 2>/dev/null || true; fi
	if command -v dnf >/dev/null; then sudo_if_needed dnf remove -y openrig 2>/dev/null || true; fi
	if command -v yum >/dev/null; then sudo_if_needed yum remove -y openrig 2>/dev/null || true; fi
	if command -v zypper >/dev/null; then sudo_if_needed zypper --non-interactive remove openrig 2>/dev/null || true; fi
	if command -v pacman >/dev/null; then sudo_if_needed pacman -Rs --noconfirm openrig-bin openrig-git openrig 2>/dev/null || true; fi
	# AppImage / tarball installs
	rm -f "$HOME/.local/bin/OpenRig-"*.AppImage \
		"$HOME/.local/bin/pedalboard" \
		"$HOME/.local/share/applications/openrig.desktop" \
		"$HOME/.local/share/icons/hicolor/scalable/apps/openrig.svg"
	echo "OpenRig removed. Your presets and data are kept."
}

install_appimage() {
	# install_appimage VERSION ARCH: user-local AppImage + desktop entry
	ver=$1
	arch=$2
	echo "Installing OpenRig $ver AppImage for $arch..."
	tmp=$(mktemp -d)
	trap 'rm -rf "$tmp"' EXIT INT TERM
	down "https://github.com/$REPO/releases/download/v$ver/OpenRig-$ver-$arch.AppImage" "$tmp/OpenRig.AppImage"
	down "https://github.com/$REPO/releases/download/v$ver/OpenRig-$ver-$arch.AppImage.sha256" "$tmp/sum"
	(cd "$tmp" && sed 's| .*OpenRig|  OpenRig.AppImage|' sum >sum.local && sha256sum -c sum.local)
	mkdir -p "$HOME/.local/bin" "$HOME/.local/share/applications" "$HOME/.local/share/icons/hicolor/scalable/apps"
	install -m 0755 "$tmp/OpenRig.AppImage" "$HOME/.local/bin/OpenRig-$ver-$arch.AppImage"
	down "https://raw.githubusercontent.com/$REPO/v$ver/dist/openrig.desktop" "$HOME/.local/share/applications/openrig.desktop"
	down "https://raw.githubusercontent.com/$REPO/v$ver/dist/openrig.svg" "$HOME/.local/share/icons/hicolor/scalable/apps/openrig.svg"
	# Point the desktop entry at this exact file (portable, no PATH needed)
	sed -i "s|^Exec=pedalboard$|Exec=$HOME/.local/bin/OpenRig-$ver-$arch.AppImage|" "$HOME/.local/share/applications/openrig.desktop"
	command -v update-desktop-database >/dev/null && update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
	echo "Installed $HOME/.local/bin/OpenRig-$ver-$arch.AppImage"
	echo "Note: the app needs JACK and lilv from your system (e.g. jackd2/jack, lilv)."
}

if [ "$UNINSTALL" -eq 1 ]; then
	uninstall_everywhere
	exit 0
fi

# --- install ---

case $(uname -m) in
x86_64) KARCH=x86_64 ;;
aarch64 | arm64) KARCH=aarch64 ;;
*) echo "Unsupported CPU: $(uname -m) (OpenRig ships x86_64 and aarch64)" >&2; exit 1 ;;
esac

if [ -z "$WANT_VERSION" ]; then
	echo "Finding the latest OpenRig release..."
	TAG=$(latest_tag)
	[ -n "$TAG" ] || {
		echo "Could not read the latest release from github.com/$REPO" >&2
		exit 1
	}
else
	case $WANT_VERSION in v*) TAG=$WANT_VERSION ;; *) TAG=v$WANT_VERSION ;; esac
fi
VER=${TAG#v}
echo "Installing OpenRig $VER ($REPO)..."

OS_ID=
OS_LIKE=
if [ -r /etc/os-release ]; then
	# shellcheck disable=SC1091
	OS_ID=$(. /etc/os-release && printf '%s' "$ID")
	OS_LIKE=$(. /etc/os-release && printf '%s' "${ID_LIKE:-}")
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM

case " $OS_LIKE $OS_ID " in
*" debian "* | *" ubuntu "* | *" raspbian "*)
	case $KARCH in
	x86_64) DEBARCH=amd64 ;;
	aarch64) DEBARCH=arm64 ;;
	esac
	need apt-get
	down "https://github.com/$REPO/releases/download/$TAG/openrig_${VER}_${DEBARCH}.deb" "$tmp/openrig.deb"
	down "https://github.com/$REPO/releases/download/$TAG/openrig_${VER}_${DEBARCH}.deb.sha256" "$tmp/sum"
	(cd "$tmp" && sed 's| .*openrig|  openrig.deb|' sum >sum.local && sha256sum -c sum.local)
	sudo_if_needed apt-get update
	sudo_if_needed apt-get install -y "$tmp/openrig.deb"
	;;
*" fedora "* | *" rhel "* | *" centos "* | *" fedora" | *" rhel" | *" centos" | *" rocky "* | *" alma "*)
	case $KARCH in
	x86_64) RPMARCH=x86_64 ;;
	aarch64) RPMARCH=aarch64 ;;
	esac
	down "https://github.com/$REPO/releases/download/$TAG/openrig-$VER-1.$RPMARCH.rpm" "$tmp/openrig.rpm"
	down "https://github.com/$REPO/releases/download/$TAG/openrig-$VER-1.$RPMARCH.rpm.sha256" "$tmp/sum"
	(cd "$tmp" && sed 's| .*openrig|  openrig.rpm|' sum >sum.local && sha256sum -c sum.local)
	if command -v dnf >/dev/null; then
		sudo_if_needed dnf install -y "$tmp/openrig.rpm"
	else
		sudo_if_needed yum install -y "$tmp/openrig.rpm"
	fi
	;;
*" suse "* | *" opensuse "*)
	case $KARCH in
	x86_64) RPMARCH=x86_64 ;;
	aarch64) RPMARCH=aarch64 ;;
	esac
	down "https://github.com/$REPO/releases/download/$TAG/openrig-$VER-1.$RPMARCH.rpm" "$tmp/openrig.rpm"
	down "https://github.com/$REPO/releases/download/$TAG/openrig-$VER-1.$RPMARCH.rpm.sha256" "$tmp/sum"
	(cd "$tmp" && sed 's| .*openrig|  openrig.rpm|' sum >sum.local && sha256sum -c sum.local)
	sudo_if_needed zypper --non-interactive install --allow-unsigned-rpm "$tmp/openrig.rpm" ||
		sudo_if_needed zypper --non-interactive install "$tmp/openrig.rpm"
	;;
*" arch "*)
	if command -v yay >/dev/null; then
		yay -S --noconfirm openrig-bin
	elif command -v paru >/dev/null; then
		paru -S --noconfirm openrig-bin
	else
		echo "No AUR helper (yay/paru) found; installing the AppImage instead."
		echo "With an AUR helper: yay -S openrig-bin  (or openrig-git for latest main)"
		install_appimage "$VER" "$KARCH"
	fi
	;;
*)
	echo "No native package for '$OS_ID'; installing the AppImage."
	install_appimage "$VER" "$KARCH"
	;;
esac

echo "Done. Start JACK, then run: pedalboard"

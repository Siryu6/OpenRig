#!/bin/sh
#
# x86_64 (UEFI) disk image and its update bundle

set -eu

BOARD_DIR=$(dirname "$0")
COMMON_DIR="$BOARD_DIR/../common"

cp -f "$BOARD_DIR/grub.cfg" "$BINARIES_DIR/efi-part/EFI/BOOT/grub.cfg"
# GRUB environment block: slot A by default (see grub.cfg)
{
	printf '# GRUB Environment Block\nprimary=A\ntry=\n'
	head -c 1024 /dev/zero | tr '\0' '#'
} | head -c 1024 >"$BINARIES_DIR/efi-part/EFI/BOOT/grubenv"
cp -f "$COMMON_DIR/pedalboard.txt" "$BINARIES_DIR/pedalboard.txt"
support/scripts/genimage.sh -c "$BOARD_DIR/genimage.cfg"

echo "Disk image: $BINARIES_DIR/disk.img"

"$COMMON_DIR/make-bundle.sh" pedalboard-x86_64 rootfs.ext4=rootfs

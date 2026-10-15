#!/bin/bash
#
# Raspberry Pi 5 card image (from Buildroot's board/raspberrypi/post-image.sh)
# and its update bundle

set -euo pipefail

BOARD_DIR=$(dirname "$0")
COMMON_DIR="${BOARD_DIR}/../common"
GENIMAGE_CFG="${BINARIES_DIR}/genimage.cfg"
GENIMAGE_TMP="${BUILD_DIR}/genimage.tmp"

# Boot slot: DTBs, firmware files (config.txt, cmdline.txt, overlays), the
# kernel, extra.txt, and cmdline.base: the image's cmdline.txt, which
# updates compare the card's with to keep the arguments added to it
rm -f "${BINARIES_DIR}/rpi-firmware/pedalboard.txt" # settings partition
cp -f "${BOARD_DIR}/extra.txt" "${BINARIES_DIR}/rpi-firmware/extra.txt"
cp -f "${BINARIES_DIR}/rpi-firmware/cmdline.txt" "${BINARIES_DIR}/rpi-firmware/cmdline.base"
FILES=()
for i in "${BINARIES_DIR}"/*.dtb "${BINARIES_DIR}"/rpi-firmware/*; do
	FILES+=("${i#"${BINARIES_DIR}"/}")
done
FILES+=("$(sed -n 's/^kernel=//p' "${BINARIES_DIR}/rpi-firmware/config.txt")")

BOOT_FILES=$(printf '\\t\\t\\t"%s",\\n' "${FILES[@]}")
sed "s|#BOOT_FILES#|${BOOT_FILES}|" "${BOARD_DIR}/genimage.cfg.in" >"${GENIMAGE_CFG}"

# Settings partition
cp -f "${BOARD_DIR}/autoboot.txt" "${BINARIES_DIR}/autoboot.txt"
cp -f "${COMMON_DIR}/pedalboard.txt" "${BINARIES_DIR}/pedalboard.txt"

# Boot slot B, written by the first update (genimage makes no empty vfat)
printf 'System slot B: empty until the first update.\r\n' >"${BINARIES_DIR}/slot-b.txt"

# genimage copies the root path; the root filesystem image is already built
ROOTPATH_TMP=$(mktemp -d)
trap 'rm -rf "${ROOTPATH_TMP}"' EXIT
rm -rf "${GENIMAGE_TMP}"

genimage \
	--rootpath "${ROOTPATH_TMP}" \
	--tmppath "${GENIMAGE_TMP}" \
	--inputpath "${BINARIES_DIR}" \
	--outputpath "${BINARIES_DIR}" \
	--config "${GENIMAGE_CFG}"

echo "Card image: ${BINARIES_DIR}/sdcard.img"

"${COMMON_DIR}/make-bundle.sh" pedalboard-rpi5 rootfs.ext4=rootfs boot.vfat=boot

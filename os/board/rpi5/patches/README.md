# Patches for the Raspberry Pi 5 image

Applied by Buildroot (`BR2_GLOBAL_PATCH_DIR`), one directory per package.
Each one is a commit taken as-is from its project: drop it once the pinned
version contains it.

`linux/linux.hash` is the sha256 of the pinned raspberrypi/linux tarball
(`BR2_DOWNLOAD_FORCE_CHECK_HASHES`): update it with the commit in
`configs/pedalboard_rpi5_defconfig`.

No patch at the moment. The last one, the dwc I2S `i2s_stop` revert
(raspberrypi/linux 8946ad8626ec) for 8-output HATs, is part of rpi-6.18.y.

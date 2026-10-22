# Patches for the Raspberry Pi 5 image

Applied by Buildroot (`BR2_GLOBAL_PATCH_DIR`), one directory per package.
Each one is a commit taken as-is from its project: drop it once the pinned
version contains it.

- `linux/0001-ASoC-dwc-Revert-i2s_stop-to-the-upstream-version.patch`:
  raspberrypi/linux 94d4703ff1f1, merged in rpi-6.18.y only. On interfaces
  using several I2S data lines (HiFiBerry DAC8x and compatible 8-output
  HATs), stopping the stream left the transmitter and its DMA running, so
  after an xrun some stereo pairs could fall silent or play another pair's
  channels.

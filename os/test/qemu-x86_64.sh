#!/bin/sh
#
# Boot a copy of the x86_64 disk image in QEMU (KVM when available), as a
# mini PC with a bigger disk, a duplex sound card (Intel HDA), virtio
# graphics, network and a USB keyboard / tablet.
#
#   qemu-x86_64.sh <images dir> <work dir> [authorized_keys]
#
# The work dir keeps the disk between runs (delete disk.img for a first
# boot again; the keys are copied on a first boot only) and gets:
#   serial.log   kernel and init messages
#   serial.sock  root shell on the serial port (test copy only: the image
#                itself has no console login)
#   qmp.sock     QEMU monitor (screenshots: "screendump")
# Ports: SSH on 2222, VNC on 5900.

set -eu

images=$1
work=$2
keys=${3:-}
screen=${QEMU_SCREEN:-1280x800}
mkdir -p "$work"

if [ ! -e "$work/disk.img" ]; then
	cp "$images/disk.img" "$work/disk.img"
	# Room for the data partition made on first boot
	truncate -s 4G "$work/disk.img"
	# Kernel messages on the serial port, without "quiet"
	part="$work/disk.img@@1M"
	mcopy -n -i "$part" ::EFI/BOOT/grub.cfg "$work/grub.cfg"
	sed -i 's/ quiet loglevel=3/ console=tty0 console=ttyS0,115200/' "$work/grub.cfg"
	mcopy -o -i "$part" "$work/grub.cfg" ::EFI/BOOT/grub.cfg
	[ -z "$keys" ] || mcopy -o -i "$part" "$keys" ::authorized_keys
	# QEMU's sound card moves audio in ~10 ms steps: JACK cannot run a
	# 128 x 2 buffer on it (clients fail to activate)
	mcopy -n -i "$part" ::pedalboard.txt "$work/pedalboard.txt"
	sed -i 's/^period=.*/period=1024/; s/^periods=.*/periods=3/' "$work/pedalboard.txt"
	mcopy -o -i "$part" "$work/pedalboard.txt" ::pedalboard.txt
	# Root shell on ttyS0, written into the copy's root filesystem
	start=$(sfdisk -d "$work/disk.img" | sed -n 's/.*img2 : start= *\([0-9]*\),.*/\1/p')
	dd if="$work/disk.img" of="$work/rootfs.ext4" bs=512 skip="$start" \
		count=$(sfdisk -d "$work/disk.img" | sed -n 's/.*img2 :.*size= *\([0-9]*\),.*/\1/p') status=none
	debugfs -R "cat /etc/inittab" "$work/rootfs.ext4" 2>/dev/null >"$work/inittab"
	echo "ttyS0::respawn:/sbin/getty -n -l /bin/sh -L 115200 ttyS0 vt100" >>"$work/inittab"
	debugfs -w -R "rm /etc/inittab" "$work/rootfs.ext4" >/dev/null 2>&1
	debugfs -w -R "write $work/inittab /etc/inittab" "$work/rootfs.ext4" >/dev/null 2>&1
	dd if="$work/rootfs.ext4" of="$work/disk.img" bs=512 seek="$start" conv=notrunc status=none
	rm "$work/rootfs.ext4"
fi
[ -e "$work/OVMF_VARS.fd" ] || cp /usr/share/OVMF/OVMF_VARS_4M.fd "$work/OVMF_VARS.fd"

accel=tcg
[ -w /dev/kvm ] && accel=kvm

exec qemu-system-x86_64 \
	-machine q35,accel=$accel -cpu max -smp 4 -m 2048 \
	-drive if=pflash,format=raw,readonly=on,file=/usr/share/OVMF/OVMF_CODE_4M.fd \
	-drive if=pflash,format=raw,file="$work/OVMF_VARS.fd" \
	-drive if=virtio,format=raw,file="$work/disk.img" \
	-device virtio-vga,xres=${screen%x*},yres=${screen#*x} -display none -vnc :0 \
	-audiodev none,id=snd0 -device intel-hda -device hda-duplex,audiodev=snd0 \
	-netdev user,id=net0,hostfwd=tcp::2222-:22 -device virtio-net-pci,netdev=net0 \
	-device qemu-xhci -device usb-kbd -device usb-tablet \
	-chardev socket,id=ser0,path="$work/serial.sock",server=on,wait=off,logfile="$work/serial.log" \
	-serial chardev:ser0 \
	-qmp unix:"$work/qmp.sock",server=on,wait=off

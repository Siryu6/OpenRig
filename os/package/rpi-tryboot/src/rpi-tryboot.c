/*
 * Set the Raspberry Pi firmware's one-shot "tryboot" flag through the
 * VideoCore mailbox: the next reboot (a plain `reboot`, which shuts the
 * system down cleanly) starts the [tryboot] partition of autoboot.txt,
 * then the firmware clears the flag. `reboot "0 tryboot"` does the same
 * with systemd; BusyBox's reboot takes no argument.
 */
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/ioctl.h>
#include <unistd.h>

#define IOCTL_MBOX_PROPERTY _IOWR(100, 0, char *)
#define TAG_SET_REBOOT_FLAGS 0x00038064
#define REBOOT_FLAG_TRYBOOT 0x1
#define RESPONSE_OK 0x80000000

int main(void)
{
	uint32_t message[7] = {
		7 * sizeof(uint32_t), 0,                /* size, request */
		TAG_SET_REBOOT_FLAGS, 4, 4, REBOOT_FLAG_TRYBOOT,
		0,                                      /* end tag */
	};
	int fd = open("/dev/vcio", O_RDWR);

	if (fd < 0) {
		perror("rpi-tryboot: /dev/vcio");
		return 1;
	}
	if (ioctl(fd, IOCTL_MBOX_PROPERTY, message) < 0) {
		perror("rpi-tryboot: mailbox");
		close(fd);
		return 1;
	}
	close(fd);
	if (message[1] != RESPONSE_OK) {
		fprintf(stderr, "rpi-tryboot: refused by the firmware (0x%08x)\n",
			(unsigned)message[1]);
		return 1;
	}
	return 0;
}

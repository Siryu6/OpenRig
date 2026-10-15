################################################################################
#
# rpi-tryboot
#
################################################################################

RPI_TRYBOOT_VERSION = 1
RPI_TRYBOOT_SITE = $(BR2_EXTERNAL_PEDALBOARD_PATH)/package/rpi-tryboot/src
RPI_TRYBOOT_SITE_METHOD = local
RPI_TRYBOOT_LICENSE = GPL-3.0-or-later

define RPI_TRYBOOT_BUILD_CMDS
	$(TARGET_CC) $(TARGET_CFLAGS) $(TARGET_LDFLAGS) -Wall -Wextra \
		-o $(@D)/rpi-tryboot $(@D)/rpi-tryboot.c
endef

define RPI_TRYBOOT_INSTALL_TARGET_CMDS
	$(INSTALL) -D -m 0755 $(@D)/rpi-tryboot $(TARGET_DIR)/usr/sbin/rpi-tryboot
endef

$(eval $(generic-package))

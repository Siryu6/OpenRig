################################################################################
#
# rtpmidid
#
################################################################################

RTPMIDID_VERSION = 26.01
RTPMIDID_SITE = $(call github,davidmoreno,rtpmidid,v$(RTPMIDID_VERSION))
# The daemon (src/) is GPL-3.0+, its library (lib/, include/) LGPL-2.1+
RTPMIDID_LICENSE = GPL-3.0+ (daemon), LGPL-2.1+ (library)
RTPMIDID_LICENSE_FILES = LICENSE.md LICENSE-daemon.txt LICENSE-lib.txt
RTPMIDID_DEPENDENCIES = host-pkgconf alsa-lib avahi

# GCC >= 13: C++20 std::format, no fmt. No tests, no precompiled headers,
# the toolchain's linker (not the host's mold / lld), the version given
# here rather than `git describe` (which would find the checkout around
# the build directory)
RTPMIDID_CONF_OPTS = \
	-DENABLE_TESTS=OFF \
	-DENABLE_PCH=OFF \
	-DCPP_VERSION=20 \
	-DUSE_FMT=OFF \
	-DLDD=default \
	-DLOG_LEVEL=2 \
	-DRTPMIDID_VERSION=$(RTPMIDID_VERSION)

# No install rule upstream: the daemon only
define RTPMIDID_INSTALL_TARGET_CMDS
	$(INSTALL) -D -m 0755 $(RTPMIDID_BUILDDIR)/src/rtpmidid \
		$(TARGET_DIR)/usr/bin/rtpmidid
endef

$(eval $(cmake-package))

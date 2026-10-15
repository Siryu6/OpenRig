################################################################################
#
# pedalboard
#
################################################################################

# Built from this checkout (working tree included), not from a release
PEDALBOARD_VERSION = local
PEDALBOARD_SITE = $(BR2_EXTERNAL_PEDALBOARD_PATH)/..
PEDALBOARD_SITE_METHOD = local
PEDALBOARD_LICENSE = GPL-3.0-or-later
PEDALBOARD_DEPENDENCIES = \
	host-pkgconf \
	$(BR2_CMAKE_HOST_DEPENDENCY) \
	alsa-lib jack2 lilv libinput libxkbcommon libdrm

# Build outputs and Buildroot trees of the checkout
PEDALBOARD_OVERRIDE_SRCDIR_RSYNC_EXCLUSIONS = \
	--exclude /target --exclude /os/output --exclude /os/.cache --exclude /os/keys

PEDALBOARD_FEATURES = kms,lv2,nam,tone3000,midi,splash
ifeq ($(BR2_PACKAGE_PEDALBOARD_GPIO),y)
PEDALBOARD_FEATURES += gpio
endif

PEDALBOARD_CARGO_BUILD_OPTS = \
	--no-default-features \
	--features $(subst $(space),$(comma),$(strip $(PEDALBOARD_FEATURES)))

# The cc / cmake crates build NeuralAudio and ring with the cross compiler
# (CC / CXX); host tools keep the host compiler. alsa-sys finds alsa-lib
# through Buildroot's pkg-config, which points at the staging tree.
PEDALBOARD_CARGO_ENV = \
	HOST_CC="$(HOSTCC)" \
	HOST_CXX="$(HOSTCXX)" \
	PKG_CONFIG_ALLOW_CROSS=1 \
	TARGET_CMAKE_TOOLCHAIN_FILE="$(HOST_DIR)/share/buildroot/toolchainfile.cmake"

# The cargo infrastructure vendors the crates when it downloads a source
# archive; a local source is not downloaded, so vendor it before the build
# (cargo is ready by then, unlike at the rsync step). Needs network
# access the first time (crates are cached in the Buildroot download dir).
define PEDALBOARD_VENDOR_CRATES
	cd $(@D) && mkdir -p .cargo $(BR_CARGO_HOME) && \
		$(HOST_MAKE_ENV) CARGO_HOME=$(BR_CARGO_HOME) \
		flock $(BR_CARGO_HOME)/.br-lock \
		cargo vendor --locked VENDOR > .cargo/config.toml
endef
PEDALBOARD_PRE_CONFIGURE_HOOKS += PEDALBOARD_VENDOR_CRATES

# Install the binary built above rather than rebuilding it with cargo install
define PEDALBOARD_INSTALL_TARGET_CMDS
	$(INSTALL) -D -m 0755 \
		$(@D)/target/$(RUSTC_TARGET_NAME)/$(if $(BR2_ENABLE_DEBUG),debug,release)/pedalboard \
		$(TARGET_DIR)/usr/bin/pedalboard
	$(INSTALL) -D -m 0755 \
		$(@D)/target/$(RUSTC_TARGET_NAME)/$(if $(BR2_ENABLE_DEBUG),debug,release)/pedalboard-splash \
		$(TARGET_DIR)/usr/libexec/pedalboard/splash
endef

$(eval $(cargo-package))

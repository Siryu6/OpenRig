# Pedalboard OS images, built with Buildroot (downloaded on first use).
#
#   make -C os rpi5               Raspberry Pi 5 card image
#   make -C os x86_64             x86_64 (UEFI) disk image
#   make -C os rpi5-menuconfig    any Buildroot target, for one board
#   make -C os rpi5-pedalboard-rebuild rpi5   rebuild the app, then the image
#   make -C os qemu-x86_64        boot the x86_64 image in QEMU (SSH :2222, VNC :5900)
#   make -C os docker-rpi5        build in a container (macOS, Windows, other
#                                 Linux distributions)
#
# Outputs go to os/output/<board>/images; downloads are cached in os/.cache.
# A first build takes about 20 GB of disk and one to two hours.

BR_VERSION = 2026.02.3
BR_SHA256 = 5a59e7501b0b4ec52c41f4bfa79412320e0b37eae5f719605a258e8d0c6fc7fb

OS_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST))))
CACHE ?= $(OS_DIR)/.cache
OUTPUT ?= $(OS_DIR)/output
BR_DIR = $(CACHE)/buildroot-$(BR_VERSION)
BOARDS = rpi5 x86_64

# Shared by the boards and kept across clean builds
export BR2_DL_DIR ?= $(CACHE)/dl

BR_MAKE = $(MAKE) -C $(BR_DIR) O=$(OUTPUT)/$(1) BR2_EXTERNAL=$(OS_DIR)

.PHONY: all $(BOARDS) buildroot help
# docker-<board> and <board>-<target> are pattern rules: they cannot be .PHONY
# (make skips implicit rules for phony targets)

all: $(BOARDS)

help:
	@sed -n '3,12s/^# \{0,1\}//p' $(OS_DIR)/Makefile

buildroot: $(BR_DIR)/Makefile

$(BR_DIR)/Makefile:
	mkdir -p $(CACHE)
	curl -fL -o $(CACHE)/buildroot-$(BR_VERSION).tar.xz \
		https://buildroot.org/downloads/buildroot-$(BR_VERSION).tar.xz
	echo "$(BR_SHA256)  $(CACHE)/buildroot-$(BR_VERSION).tar.xz" | sha256sum -c -
	tar -C $(CACHE) -xf $(CACHE)/buildroot-$(BR_VERSION).tar.xz
	rm $(CACHE)/buildroot-$(BR_VERSION).tar.xz
	touch $@

# Configure again when the board's defconfig changes
$(OUTPUT)/%/.config: $(OS_DIR)/configs/pedalboard_%_defconfig | $(BR_DIR)/Makefile
	$(call BR_MAKE,$*) pedalboard_$*_defconfig

$(BOARDS): %: $(OUTPUT)/%/.config
	$(call BR_MAKE,$*)

# <board>-<buildroot target>, e.g. rpi5-menuconfig, x86_64-linux-rebuild.
# One pattern rule per board: a rule with several pattern targets would be
# taken as one recipe making all of them.
define BOARD_TARGET
$(1)-%: $$(OUTPUT)/$(1)/.config
	$$(call BR_MAKE,$(1)) $$*
endef
$(foreach b,$(BOARDS),$(eval $(call BOARD_TARGET,$(b))))

# The checkout is mounted at the same path, so Buildroot's absolute paths
# stay valid inside and outside the container. Files are written as the
# calling user (rootless podman, also behind a `docker` command, needs
# keep-id for that).
DOCKER ?= docker
DOCKER_USER = $(if $(findstring podman,$(shell $(DOCKER) --version 2>&1)),--userns=keep-id,--user $(shell id -u):$(shell id -g))
DOCKER_TTY = $(if $(shell test -t 0 && echo y),-it)

docker-%:
	$(DOCKER) build -t pedalboard-os-builder $(OS_DIR)
	$(DOCKER) run --rm $(DOCKER_TTY) $(DOCKER_USER) \
		$(if $(PEDALBOARD_OS_VERSION),-e PEDALBOARD_OS_VERSION=$(PEDALBOARD_OS_VERSION)) \
		-v $(abspath $(OS_DIR)/..):$(abspath $(OS_DIR)/..) \
		-w $(OS_DIR) pedalboard-os-builder make $*

# Boot the x86_64 image in QEMU, in a container (KVM when /dev/kvm exists):
# SSH on 127.0.0.1:2222 with QEMU_KEYS (a public key file), VNC on :5900.
# The disk is kept in output/x86_64/qemu; delete it for a first boot again.
# QEMU_SCREEN sets the display size, e.g. 1024x600 for a 10" screen.
QEMU_DIR = $(OUTPUT)/x86_64/qemu
QEMU_KEYS ?=
QEMU_SCREEN ?= 1280x800

.PHONY: qemu-x86_64
qemu-x86_64:
	$(DOCKER) build -t pedalboard-os-qemu $(OS_DIR)/test
	mkdir -p $(QEMU_DIR)
	$(if $(QEMU_KEYS),cp $(QEMU_KEYS) $(QEMU_DIR)/authorized_keys)
	$(DOCKER) run --rm $(DOCKER_TTY) $(DOCKER_USER) \
		$(if $(wildcard /dev/kvm),--device /dev/kvm) -p 127.0.0.1:2222:2222 -p 127.0.0.1:5900:5900 \
		-e QEMU_SCREEN=$(QEMU_SCREEN) \
		-v $(OUTPUT)/x86_64:$(OUTPUT)/x86_64 -v $(OS_DIR)/test:$(OS_DIR)/test:ro \
		pedalboard-os-qemu $(OS_DIR)/test/qemu-x86_64.sh $(OUTPUT)/x86_64/images \
		$(QEMU_DIR) $(if $(QEMU_KEYS),$(QEMU_DIR)/authorized_keys)

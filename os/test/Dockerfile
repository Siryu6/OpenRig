# Boots the x86_64 image in QEMU: `make -C os qemu-x86_64`
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
        e2fsprogs fdisk mtools ovmf qemu-system-x86 qemu-utils \
    && rm -rf /var/lib/apt/lists/*

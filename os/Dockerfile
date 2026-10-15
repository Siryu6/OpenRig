# Build environment for the pedalboard OS images: `make -C os docker-rpi5`
FROM debian:bookworm-slim

# Buildroot's host requirements (manual, "System requirements")
RUN apt-get update && apt-get install -y --no-install-recommends \
        bc bison build-essential bzip2 ca-certificates cpio curl file flex \
        git gzip libncurses-dev patch perl python3 rsync sed tar unzip \
        util-linux wget which xz-utils \
    && rm -rf /var/lib/apt/lists/*

# Buildroot refuses to build without a writable HOME
ENV HOME=/tmp

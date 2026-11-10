# Cross build image for the 64-bit Raspberry Pi target.
#
# Cross.toml names this image explicitly. cross 0.2.5 derives the name of a
# `pre-build` image from the workspace directory and drops its digits, so a
# checkout such as `sans-22` produced the invalid tag `cross-custom-sans-:...`.
# Because Cross.toml sets `image`, cross passes that same name as
# CROSS_BASE_IMAGE, so the upstream base image is pinned here instead.
FROM ghcr.io/cross-rs/aarch64-unknown-linux-gnu:0.2.5

ARG CROSS_DEB_ARCH=arm64
RUN dpkg --add-architecture "$CROSS_DEB_ARCH" && \
    apt-get update && \
    apt-get --assume-yes install "libv4l-dev:$CROSS_DEB_ARCH"

# syntax=docker/dockerfile:1.7
#
# Build from the repository root:
#   docker build --target runtime -f docker/Dockerfile -t rouch:dev .
#
# The default runtime image is intentionally a nested-compositor image. It
# does not claim or assume direct DRM/KMS ownership from inside Docker.

ARG RUST_VERSION=1.87.0

FROM rust:$RUST_VERSION-bookworm AS toolchain

ENV CARGO_TERM_COLOR=always \
    RUST_BACKTRACE=1

RUN apt-get update \
    && apt-get install --yes --no-install-recommends \
        ca-certificates \
        pkg-config \
        build-essential \
        libwayland-dev \
        libxkbcommon-dev \
        libegl1-mesa-dev \
        libgl1-mesa-dev \
        libvulkan-dev \
        libdrm-dev \
        libgbm-dev \
        libudev-dev \
        libinput-dev \
        libseat-dev \
        wayland-protocols \
        libx11-dev \
        libx11-xcb-dev \
        libxcb1-dev \
        libxcb-render0-dev \
        libxcb-shape0-dev \
        libxcb-xfixes0-dev \
        libxrandr-dev \
        libxinerama-dev \
        libxcursor-dev \
        libxi-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /workspace

# Cargo.lock is copied before the source to make dependency-layer caching
# deterministic. The source tree and bundled wallpaper are the complete build
# input for this crate.
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY Wallpaper.webp Icon-Composer.webp ./

FROM toolchain AS check
RUN cargo check --locked --all-targets

FROM toolchain AS test
RUN cargo test --locked --all-targets

# Release builds intentionally depend on the test stage. Building the default
# target therefore verifies the same locked source before producing the binary.
FROM test AS build
RUN cargo build --locked --release

FROM test AS dev
CMD ["cargo", "run", "--locked"]

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install --yes --no-install-recommends \
        ca-certificates \
        libwayland-client0 \
        libxkbcommon0 \
        libegl1 \
        libgl1 \
        libvulkan1 \
        libdrm2 \
        libgbm1 \
        libudev1 \
        libinput10 \
        libseat1 \
        libgl1-mesa-dri \
        mesa-vulkan-drivers \
        xwayland \
        dbus-user-session \
        xdg-desktop-portal \
        xdg-desktop-portal-gtk \
        libx11-6 \
        libx11-xcb1 \
        libxcb1 \
        libxcb-render0 \
        libxcb-shape0 \
        libxcb-xfixes0 \
        libxrandr2 \
        libxinerama1 \
        libxcursor1 \
        libxi6 \
        flatpak \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 10001 --shell /usr/sbin/nologin rouch

COPY --from=build /workspace/target/release/rouch /usr/local/bin/rouch
# The current wallpaper lookup checks next to the executable and its parents.
COPY --from=build /workspace/Wallpaper.webp /usr/local/bin/Wallpaper.webp
COPY --from=build /workspace/Icon-Composer.webp /usr/local/bin/Icon-Composer.webp

USER rouch
WORKDIR /home/rouch
ENTRYPOINT ["/usr/local/bin/rouch"]

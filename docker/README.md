# Rouch in Docker

The Dockerfile has reproducible inputs for the Rust build: Rust 1.87.0,
Cargo.lock, and an explicitly listed Debian dependency set. Build from the
repository root so the source and bundled assets are in the build context:

    docker build --target runtime -f docker/Dockerfile -t rouch:dev .

The default runtime target runs cargo test --locked --all-targets before the
release build. Other targets are useful in CI:

    docker build --target check -f docker/Dockerfile -t rouch-check .
    docker build --target test -f docker/Dockerfile -t rouch-test .
    docker build --target dev -f docker/Dockerfile -t rouch-dev .

The dev target is an image with the source at /workspace. Mount the checkout
and Cargo caches when iterating:

    docker run --rm -it \
      -v "$PWD:/workspace" \
      -v "$HOME/.cargo/registry:/usr/local/cargo/registry" \
      -v "$HOME/.cargo/git:/usr/local/cargo/git" \
      -w /workspace \
      rouch-dev

The container is not a native DRM/KMS session. Docker does not automatically
grant access to /dev/dri, a Linux virtual terminal, libseat, or physical
input devices, and this image never presents that as supported. Rouch's first
backend is a nested Winit/OpenGL path, so graphical development requires an
existing host Wayland or X11 session and the corresponding socket/display
mounts. For example, a Wayland host can use a command shaped like:

    docker run --rm -it \
      --user "$(id -u):$(id -g)" \
      -e XDG_RUNTIME_DIR \
      -e WAYLAND_DISPLAY \
      -v "$XDG_RUNTIME_DIR:$XDG_RUNTIME_DIR" \
      -v "$PWD:/workspace" \
      -w /workspace \
      rouch-dev

The socket path and permissions vary by host. X11 needs DISPLAY, the X11
socket, and an appropriate authorization cookie; do not use xhost + as a
general workaround. Mapping /dev/dri is an explicit experiment for a trusted
environment, not a promise that a container can own a display or provide a
real DRM compositor.

Flatpak gallery actions also need the host user's session D-Bus and Flatpak
runtime mounts. Without those, the gallery intentionally falls back to its
cached catalog and local `.desktop` entries; the container never pretends that
an install or launch succeeded.

The image contains the DRM/KMS, libseat, libinput, Vulkan, EGL/OpenGL,
XWayland and portal development/runtime libraries needed by the native-session
build. The default container command is still `rouch` in nested mode, because
the container does not own a VT or seat. Native DRM testing is an explicit
trusted experiment and must provide `/dev/dri`, a real Linux seat and the
matching permissions from the host.

The graphics policy is Vulkan-first and falls back to OpenGL, then opaque
software. The current nested Winit surface is OpenGL/EGL; the native Vulkan
surface is kept behind the session backend and must be validated on the target
Mint, Ubuntu or Arch host.

For offline or fully hermetic builds, pre-populate Cargo's registry/git
caches and use the same pinned Rust image in an environment with network
access disabled. Docker alone does not pin the mutable Debian package
repository snapshot; pin the base image by digest in release CI when
bit-for-bit system-image reproducibility is required.

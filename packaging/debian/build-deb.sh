#!/usr/bin/env bash
# Build a self-contained .deb from this checkout without mutating the source.
set -Eeuo pipefail
IFS=$'\n\t'

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
ROOT_DIR="$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)"
SOURCE_DIR="${ROUCH_SOURCE_DIR:-$ROOT_DIR}"
VERSION="${ROUCH_DEB_VERSION:-0.1.0}"
BINARY_PATH="${ROUCH_BINARY_PATH:-}"
OUTPUT_DIR="${ROUCH_OUTPUT_DIR:-$ROOT_DIR/dist}"

die() {
    printf 'Rouch .deb builder: error: %s\n' "$*" >&2
    exit 1
}

command -v dpkg-deb >/dev/null 2>&1 || die "dpkg-deb is required"
[[ "$VERSION" =~ ^[0-9][0-9A-Za-z.+:~-]*$ ]] || die "invalid Debian version: $VERSION"
[[ -f "$SOURCE_DIR/Cargo.toml" ]] || die "Cargo.toml not found in $SOURCE_DIR"

case "$(uname -m)" in
    x86_64|amd64) DEB_ARCH=amd64 ;;
    aarch64|arm64) DEB_ARCH=arm64 ;;
    armv7l|armv7) DEB_ARCH=armhf ;;
    *) die "unsupported architecture; set up a release builder for this CPU" ;;
esac

if [[ -z "$BINARY_PATH" ]]; then
    command -v cargo >/dev/null 2>&1 || die "cargo is required when ROUCH_BINARY_PATH is unset"
    cargo build --locked --release --features native-session --manifest-path "$SOURCE_DIR/Cargo.toml"
    BINARY_PATH="$SOURCE_DIR/target/release/rouch"
fi
[[ -f "$BINARY_PATH" && -s "$BINARY_PATH" ]] || die "binary is missing or empty: $BINARY_PATH"

TEMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/rouch-deb.XXXXXXXX")"
trap 'rm -rf -- "$TEMP_ROOT"' EXIT
PKG_ROOT="$TEMP_ROOT/root"
mkdir -p "$PKG_ROOT/DEBIAN"

install -Dm755 "$BINARY_PATH" "$PKG_ROOT/usr/bin/rouch"
install -Dm755 "$SCRIPT_DIR/../common/rouch-session" "$PKG_ROOT/usr/bin/rouch-session"
install -Dm755 "$SCRIPT_DIR/../common/rouch-nested" "$PKG_ROOT/usr/bin/rouch-nested"
install -Dm644 "$SCRIPT_DIR/../common/rouch.desktop" "$PKG_ROOT/usr/share/applications/rouch.desktop"
install -Dm644 "$SCRIPT_DIR/../common/rouch-wayland-session.desktop" \
    "$PKG_ROOT/usr/share/wayland-sessions/rouch.desktop"
install -Dm644 "$SCRIPT_DIR/../common/rouch.service" \
    "$PKG_ROOT/usr/lib/systemd/user/rouch.service"
install -Dm644 "$SOURCE_DIR/Wallpaper.webp" \
    "$PKG_ROOT/usr/share/rouch/interface-stage1/Wallpaper.webp"
if [[ -f "$SOURCE_DIR/Icon-Composer.webp" ]]; then
    install -Dm644 "$SOURCE_DIR/Icon-Composer.webp" \
        "$PKG_ROOT/usr/share/rouch/interface-stage1/Icon-Composer.webp"
fi

sed -e "s/^Version: .*/Version: $VERSION/" \
    -e "s/^Architecture: .*/Architecture: $DEB_ARCH/" \
    "$SCRIPT_DIR/control" > "$PKG_ROOT/DEBIAN/control"
for script in preinst postinst prerm postrm; do
    install -Dm755 "$SCRIPT_DIR/$script" "$PKG_ROOT/DEBIAN/$script"
done

mkdir -p "$OUTPUT_DIR"
OUTPUT_PATH="${ROUCH_OUTPUT:-$OUTPUT_DIR/rouch_${VERSION}_${DEB_ARCH}.deb}"
[[ ! -e "$OUTPUT_PATH" ]] || die "refusing to overwrite existing output: $OUTPUT_PATH"
dpkg-deb --build --root-owner-group "$PKG_ROOT" "$OUTPUT_PATH"
printf 'built %s\n' "$OUTPUT_PATH"

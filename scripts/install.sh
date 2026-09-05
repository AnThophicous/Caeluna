#!/usr/bin/env bash
#
# Caelune Linux installer.
#
# This file is self-contained so it works when piped to bash. Downloads and
# builds are staged in a temporary directory. Existing targets are replaced
# only after confirmation (or --yes).

set -Eeuo pipefail
IFS=$'\n\t'

readonly PRODUCT_NAME="Caelune"
readonly COMPATIBILITY_NAME="Rouch"
readonly SCRIPT_NAME="Caelune installer"
readonly DEFAULT_REPOSITORY="AnThophicous/Caeluna"
readonly DEFAULT_INTERFACE_ASSET="rouch-interface-stage1.tar.gz"
readonly DEFAULT_BINARY_PREFIX="rouch-linux"
readonly LOW_END_MEMORY_MIB=4096
readonly INSTALLER_LOG_NAME="caelune-installer.log"

DRY_RUN=0
ASSUME_YES=0
NO_BUILD=0
SKIP_DRIVERS=0
SKIP_FLATHUB=0
DIAGNOSE_ONLY=0
INSTALL_PREFIX=""
DESKTOP_DIR_OVERRIDE=""
SESSION_DIR_OVERRIDE=""
SYSTEMD_USER_DIR_OVERRIDE=""
SOURCE_DIR=""
BINARY_PATH=""
INTERFACE_DIR=""
WORK_DIR=""
DISTRO_ID="unknown"
DISTRO_LIKE=""
DISTRO_NAME="unknown Linux"
DISTRO_VERSION="unknown"
DISTRO_SUPPORTED=0
PACKAGE_MANAGER=""
INSTALL_PACKAGES=""
MISSING_PACKAGES=""
PRIVILEGE_COMMAND=""
FILE_PRIVILEGED=0
MUTATION_STARTED=0
BINARY_BACKED_UP=0
DESKTOP_BACKED_UP=0
INTERFACE_BACKED_UP=0
SESSION_LAUNCHER_BACKED_UP=0
NESTED_LAUNCHER_BACKED_UP=0
SESSION_BACKED_UP=0
USER_SERVICE_BACKED_UP=0

HOST_ARCH="unknown"
HOST_KERNEL="unknown"
HOST_CPU_MODEL="unknown"
HOST_CPU_THREADS=1
HOST_MEMORY_MIB=0
HOST_GPU_INFO="unavailable"
GPU_VENDOR="unknown"
GPU_MODEL="unknown"
GPU_DRIVER="unknown"
GPU_DRIVER_STATUS="unknown"
VULKAN_STATUS="unknown"
OPENGL_STATUS="unknown"
SESSION_STATUS="unknown"
TARGET_USER=""
TARGET_HOME=""
TARGET_UID=""
TARGET_CONFIG_DIR=""
TARGET_STATE_DIR=""
TARGET_CACHE_DIR=""
LOG_FILE=""
BINARY_INSTALLED=0
INTERFACE_INSTALLED=0
SESSION_LAUNCHER_INSTALLED=0
NESTED_LAUNCHER_INSTALLED=0
SESSION_INSTALLED=0
USER_SERVICE_INSTALLED=0
BIN_DIR=""
SHARE_DIR=""
DESKTOP_DIR=""
SESSION_DIR=""
SYSTEMD_USER_DIR=""
BINARY_TARGET=""
DESKTOP_TARGET=""
INTERFACE_TARGET=""
SESSION_LAUNCHER_TARGET=""
NESTED_LAUNCHER_TARGET=""
SESSION_TARGET=""
USER_SERVICE_TARGET=""
BINARY_BACKUP=""
DESKTOP_BACKUP=""
INTERFACE_BACKUP=""
SESSION_LAUNCHER_BACKUP=""
NESTED_LAUNCHER_BACKUP=""
SESSION_BACKUP=""
USER_SERVICE_BACKUP=""

# Reading through printenv keeps optional variables harmless under -u.
env_value() {
    printenv "$1" 2>/dev/null || true
}

alias_env() {
    local legacy="$1"
    local modern="$2"
    local value
    if [[ -z "$(env_value "$legacy")" ]]; then
        value="$(env_value "$modern")"
        if [[ -n "$value" ]]; then
            export "$legacy=$value"
        fi
    fi
}

for env_pair in \
    "ROUCH_GITHUB_REPO CAELUNE_GITHUB_REPO" \
    "ROUCH_VERSION CAELUNE_VERSION" \
    "ROUCH_PREFIX CAELUNE_PREFIX" \
    "ROUCH_DESKTOP_DIR CAELUNE_DESKTOP_DIR" \
    "ROUCH_SESSION_DIR CAELUNE_SESSION_DIR" \
    "ROUCH_SYSTEMD_USER_DIR CAELUNE_SYSTEMD_USER_DIR" \
    "ROUCH_SOURCE_DIR CAELUNE_SOURCE_DIR" \
    "ROUCH_BINARY_PATH CAELUNE_BINARY_PATH" \
    "ROUCH_BINARY_URL CAELUNE_BINARY_URL" \
    "ROUCH_BINARY_ASSET CAELUNE_BINARY_ASSET" \
    "ROUCH_BINARY_SHA256 CAELUNE_BINARY_SHA256" \
    "ROUCH_SOURCE_URL CAELUNE_SOURCE_URL" \
    "ROUCH_SOURCE_SHA256 CAELUNE_SOURCE_SHA256" \
    "ROUCH_INTERFACE_DIR CAELUNE_INTERFACE_DIR" \
    "ROUCH_INTERFACE_URL CAELUNE_INTERFACE_URL" \
    "ROUCH_INTERFACE_ASSET CAELUNE_INTERFACE_ASSET" \
    "ROUCH_INTERFACE_SHA256 CAELUNE_INTERFACE_SHA256"; do
    IFS=' ' read -r legacy_env modern_env <<< "$env_pair"
    alias_env "$legacy_env" "$modern_env"
done

INSTALL_PREFIX="$(env_value ROUCH_PREFIX)"
DESKTOP_DIR_OVERRIDE="$(env_value ROUCH_DESKTOP_DIR)"
SESSION_DIR_OVERRIDE="$(env_value ROUCH_SESSION_DIR)"
SYSTEMD_USER_DIR_OVERRIDE="$(env_value ROUCH_SYSTEMD_USER_DIR)"
SOURCE_DIR="$(env_value ROUCH_SOURCE_DIR)"
BINARY_PATH="$(env_value ROUCH_BINARY_PATH)"
INTERFACE_DIR="$(env_value ROUCH_INTERFACE_DIR)"

if [[ -t 1 && -z "$(env_value NO_COLOR)" ]]; then
    COLOR_CYAN=$'\033[36m'
    COLOR_GREEN=$'\033[32m'
    COLOR_YELLOW=$'\033[33m'
    COLOR_RED=$'\033[31m'
    COLOR_RESET=$'\033[0m'
else
    COLOR_CYAN=""
    COLOR_GREEN=""
    COLOR_YELLOW=""
    COLOR_RED=""
    COLOR_RESET=""
fi

banner() {
    cat <<'EOF'
  ____            _
 / ___|__ _  ___| |_   _ _ __   ___
| |   / _` |/ _ \ | | | | '_ \ / _ \
| |__| (_| |  __/ | |_| | | | |  __/
 \____\__,_|\___|_|\__,_|_| |_|\___|

 Caelune - instalador do desktop Wayland Liquid Glass
EOF
}

section() {
    printf '\n%s== %s ==%s\n' "$COLOR_CYAN" "$*" "$COLOR_RESET"
}

success() {
    printf '%s[ok]%s %s\n' "$COLOR_GREEN" "$COLOR_RESET" "$*"
}

die() {
    printf '%s%s: erro:%s %s\n' "$COLOR_RED" "$SCRIPT_NAME" "$COLOR_RESET" "$*" >&2
    exit 1
}

warn() {
    printf '%s%s: aviso:%s %s\n' "$COLOR_YELLOW" "$SCRIPT_NAME" "$COLOR_RESET" "$*" >&2
}

info() {
    printf '%s: %s\n' "$SCRIPT_NAME" "$*"
}

if [[ "$(env_value CAELUNE_BANNER_SHOWN)" != 1 ]]; then
    banner
fi

usage() {
    cat <<'EOF'
Install Caelune on a Linux host.
Officially supported distributions: Linux Mint, Ubuntu, and Arch Linux.

Options:
  --dry-run       print the plan without downloading, building, installing,
                  changing packages, or changing files
  --yes           confirm package installation and replacement of existing
                  Caelune files without prompting
  --no-build      install a prebuilt binary instead of compiling from source
  --skip-drivers  install the desktop without attempting GPU driver repair
  --skip-flatpak  do not add Flathub to the current user's Flatpak remotes
  --diagnose      inspect the host and print recommendations without changes
  -h, --help      show this help

Useful environment overrides:
  CAELUNE_GITHUB_REPO=owner/repo (ROUCH_GITHUB_REPO also works)
  CAELUNE_INSTALLER_REF=main    branch/tag used to fetch installer.sh
  CAELUNE_VERSION=latest|tag
  CAELUNE_USER=login           user that receives the initial settings
  CAELUNE_PREFIX=/absolute/install/prefix
  ROUCH_GITHUB_REPO=owner/repo
  ROUCH_VERSION=latest|tag
  ROUCH_PREFIX=/absolute/install/prefix
  ROUCH_BINARY_URL=https://.../rouch-linux-x86_64
  ROUCH_BINARY_PATH=/path/to/rouch
  ROUCH_INTERFACE_URL=https://.../rouch-interface-stage1.tar.gz
  ROUCH_INTERFACE_DIR=/path/to/local/stage1
  ROUCH_SOURCE_URL=https://.../source.tar.gz
  ROUCH_SOURCE_DIR=/path/to/checkout
  ROUCH_DESKTOP_DIR=/absolute/applications/directory
  ROUCH_SESSION_DIR=/absolute/wayland-sessions/directory
  ROUCH_SYSTEMD_USER_DIR=/absolute/systemd/user/directory

The default repository is AnThophicous/Caeluna. When it has no release asset,
the installer falls back to the main source archive.

An optional release layout can publish:
  - rouch-interface-stage1.tar.gz
  - rouch-linux-{x86_64,aarch64,armv7}
  - a source archive at GitHub's tag/main archive endpoint

Override URLs or asset names when the release pipeline uses another layout.
Use CAELUNE_GITHUB_REPO to install a fork.

The installer keeps both launch contracts:
  - rouch-nested --nested  (existing desktop development session)
  - rouch-session --session (display-manager Wayland session)
The systemd user unit is installed disabled and is never enabled implicitly.

The installer never changes the active display manager, never installs a
proprietary GPU driver silently, and never enables a background service without
showing the action first.
EOF
}

while (($# > 0)); do
    case "$1" in
        --dry-run)
            DRY_RUN=1
            ;;
        --yes)
            ASSUME_YES=1
            ;;
        --no-build)
            NO_BUILD=1
            ;;
        --skip-drivers)
            SKIP_DRIVERS=1
            ;;
        --skip-flatpak)
            SKIP_FLATHUB=1
            ;;
        --diagnose)
            DIAGNOSE_ONLY=1
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            die "unknown option '$1' (use --help)"
            ;;
    esac
    shift
done

[[ "$(uname -s)" == "Linux" ]] || die "this installer only supports Linux"

has_command() {
    command -v "$1" >/dev/null 2>&1
}

require_command() {
    has_command "$1" || die "required command '$1' was not found"
}

command_line() {
    local part
    printf '  +'
    for part in "$@"; do
        printf ' %q' "$part"
    done
    printf '\n'
}

run() {
    command_line "$@"
    if ((DRY_RUN)); then
        return 0
    fi
    "$@"
}

run_privileged() {
    if [[ -n "$PRIVILEGE_COMMAND" ]]; then
        command_line "$PRIVILEGE_COMMAND" "$@"
        if ((DRY_RUN)); then
            return 0
        fi
        "$PRIVILEGE_COMMAND" "$@"
    else
        run "$@"
    fi
}

run_file() {
    if ((FILE_PRIVILEGED)); then
        run_privileged "$@"
    else
        run "$@"
    fi
}

confirm() {
    local prompt="$1"
    local answer

    if ((DRY_RUN || ASSUME_YES)); then
        return 0
    fi
    if [[ ! -r /dev/tty ]]; then
        die "$prompt; stdin has no terminal, rerun with --yes"
    fi
    printf '%s [y/N] ' "$prompt" >/dev/tty
    if ! read -r answer </dev/tty; then
        die "could not read confirmation from /dev/tty; rerun with --yes"
    fi
    case "$answer" in
        y|Y|yes|YES|Yes)
            ;;
        *)
            die "operation cancelled; no changes were made"
            ;;
    esac
}

on_exit() {
    local exit_code=$?
    trap - EXIT
    if ((exit_code != 0 && MUTATION_STARTED)); then
        printf '%s: attempting rollback of the staged installation...\n' "$SCRIPT_NAME" >&2
        rollback || warn "rollback was incomplete; inspect the listed targets manually"
    fi
    cleanup
    exit "$exit_code"
}

cleanup() {
    if [[ -n "$WORK_DIR" && -d "$WORK_DIR" ]]; then
        # WORK_DIR is the exact directory returned by mktemp -d.
        rm -rf -- "$WORK_DIR"
    fi
}

restore_file() {
    local target="$1"
    local backup="$2"
    local had_backup="$3"

    restore_path "$target" "$backup" "$had_backup" 0755
}

restore_path() {
    local target="$1"
    local backup="$2"
    local had_backup="$3"
    local mode="$4"

    if ((had_backup)); then
        run_file install -m "$mode" -- "$backup" "$target"
    elif [[ -e "$target" || -L "$target" ]]; then
        run_file rm -f -- "$target"
    fi
}

restore_desktop() {
    local target="$1"
    local backup="$2"
    local had_backup="$3"

    restore_path "$target" "$backup" "$had_backup" 0644
}

rollback() {
    set +e
    if ((INTERFACE_INSTALLED)); then
        if [[ -d "$INTERFACE_TARGET" && ! -L "$INTERFACE_TARGET" ]]; then
            run_file rm -rf -- "$INTERFACE_TARGET"
        fi
    fi
    if ((INTERFACE_BACKED_UP)) && [[ -d "$INTERFACE_BACKUP" ]]; then
        run_file mv -- "$INTERFACE_BACKUP" "$INTERFACE_TARGET"
    fi
    restore_path "$USER_SERVICE_TARGET" "$USER_SERVICE_BACKUP" "$USER_SERVICE_BACKED_UP" 0644
    restore_path "$SESSION_TARGET" "$SESSION_BACKUP" "$SESSION_BACKED_UP" 0644
    restore_path "$NESTED_LAUNCHER_TARGET" "$NESTED_LAUNCHER_BACKUP" \
        "$NESTED_LAUNCHER_BACKED_UP" 0755
    restore_path "$SESSION_LAUNCHER_TARGET" "$SESSION_LAUNCHER_BACKUP" \
        "$SESSION_LAUNCHER_BACKED_UP" 0755
    if [[ -n "$DESKTOP_TARGET" ]]; then
        restore_desktop "$DESKTOP_TARGET" "$DESKTOP_BACKUP" "$DESKTOP_BACKED_UP"
    fi
    if [[ -n "$BINARY_TARGET" ]]; then
        restore_file "$BINARY_TARGET" "$BINARY_BACKUP" "$BINARY_BACKED_UP"
    fi
}

detect_distribution_legacy() {
    if [[ -r /etc/os-release ]]; then
        # /etc/os-release is the standard administrator-owned identification
        # file. Read only the two fields used below.
        DISTRO_ID="$(sed -n 's/^ID=//p' /etc/os-release | tr -d '"')"
        DISTRO_LIKE="$(sed -n 's/^ID_LIKE=//p' /etc/os-release | tr -d '"')"
        [[ -n "$DISTRO_ID" ]] || DISTRO_ID="unknown"
    fi

    # Official support is intentionally narrow. Package-manager similarity is
    # not enough: a Debian/Fedora/Manjaro install can have subtly different
    # seat, input and display-manager integration.
    if [[ "$DISTRO_ID" == ubuntu || "$DISTRO_ID" == linuxmint ]]; then
        has_command apt-get ||
            die "'$DISTRO_ID' is supported, but apt-get is missing; install apt or use the manual package flow"
        PACKAGE_MANAGER="apt"
        if ((NO_BUILD)); then
            INSTALL_PACKAGES="libwayland-client0 libwayland-server0 libxkbcommon0 libegl1 libgl1 libvulkan1 libdrm2 libgbm1 libinput10 libseat1 seatd xwayland mesa-vulkan-drivers libgl1-mesa-dri flatpak dbus-user-session xdg-desktop-portal xdg-desktop-portal-gtk"
        else
            INSTALL_PACKAGES="build-essential pkg-config cargo rustc libwayland-dev libxkbcommon-dev libegl1-mesa-dev libgl1-mesa-dev libvulkan-dev libdrm-dev libgbm-dev libudev-dev libinput-dev libseat-dev wayland-protocols xwayland mesa-vulkan-drivers libgl1-mesa-dri flatpak seatd dbus-user-session xdg-desktop-portal xdg-desktop-portal-gtk"
        fi
    elif [[ "$DISTRO_ID" == arch ]]; then
        has_command pacman ||
            die "'arch' is supported, but pacman is missing; repair the base installation first"
        PACKAGE_MANAGER="pacman"
        if ((NO_BUILD)); then
            INSTALL_PACKAGES="wayland libxkbcommon libdrm libinput seatd xorg-xwayland vulkan-icd-loader vulkan-swrast mesa libglvnd flatpak xdg-desktop-portal xdg-desktop-portal-gtk dbus"
        else
            INSTALL_PACKAGES="base-devel rust cargo pkgconf wayland-protocols wayland libxkbcommon libdrm libinput seatd xorg-xwayland vulkan-headers vulkan-icd-loader vulkan-swrast mesa libglvnd flatpak xdg-desktop-portal xdg-desktop-portal-gtk dbus"
        fi
    else
        die "unsupported distribution '$DISTRO_ID' (ID_LIKE='$DISTRO_LIKE'). Official Rouch support is Linux Mint, Ubuntu, and Arch only; no changes were made"
    fi
}

read_os_release_value() {
    local key="$1"
    local fallback="$2"
    local value=""

    if [[ -r /etc/os-release ]]; then
        value="$(sed -n "s/^${key}=//p" /etc/os-release | head -n 1)"
        value="${value#\"}"
        value="${value%\"}"
        value="${value#\'}"
        value="${value%\'}"
    fi
    if [[ -n "$value" ]]; then
        printf '%s\n' "$value"
    else
        printf '%s\n' "$fallback"
    fi
}

detect_hardware() {
    local gpu_lower

    HOST_GPU_INFO="unavailable"
    GPU_VENDOR="unknown"
    GPU_MODEL="unavailable"
    GPU_DRIVER="unavailable"
    GPU_DRIVER_STATUS="unknown"
    HOST_ARCH="$(uname -m 2>/dev/null || printf 'unknown')"
    HOST_KERNEL="$(uname -r 2>/dev/null || printf 'unknown')"
    if [[ -r /proc/cpuinfo ]]; then
        HOST_CPU_MODEL="$(sed -n 's/^model name[[:space:]]*:[[:space:]]*//p' /proc/cpuinfo | head -n 1)"
        if [[ -z "$HOST_CPU_MODEL" ]]; then
            HOST_CPU_MODEL="$(sed -n 's/^Hardware[[:space:]]*:[[:space:]]*//p' /proc/cpuinfo | head -n 1)"
        fi
    fi
    if [[ -z "$HOST_CPU_MODEL" ]] && has_command sysctl; then
        HOST_CPU_MODEL="$(sysctl -n hw.model 2>/dev/null || true)"
    fi
    [[ -n "$HOST_CPU_MODEL" ]] || HOST_CPU_MODEL="unavailable"

    HOST_CPU_THREADS="$(getconf _NPROCESSORS_ONLN 2>/dev/null || printf '1')"
    [[ "$HOST_CPU_THREADS" =~ ^[0-9]+$ && "$HOST_CPU_THREADS" -gt 0 ]] || HOST_CPU_THREADS=1
    if [[ -r /proc/meminfo ]]; then
        HOST_MEMORY_MIB="$(awk '/^MemTotal:/ { print int($2 / 1024); exit }' /proc/meminfo)"
    fi
    [[ "$HOST_MEMORY_MIB" =~ ^[0-9]+$ ]] || HOST_MEMORY_MIB=0

    if has_command lspci; then
        HOST_GPU_INFO="$(lspci -nn 2>/dev/null | awk '/VGA compatible controller|3D controller|Display controller/ { printf "%s%s", separator, $0; separator="; " }' || true)"
        GPU_DRIVER="$(lspci -k 2>/dev/null | sed -n 's/^[[:space:]]*Kernel driver in use:[[:space:]]*//p' | head -n 1 || true)"
    fi
    [[ -n "$HOST_GPU_INFO" ]] || HOST_GPU_INFO="unavailable"
    [[ -n "$GPU_DRIVER" ]] || GPU_DRIVER="unavailable"

    gpu_lower="$(printf '%s' "$HOST_GPU_INFO" | tr '[:upper:]' '[:lower:]')"
    case "$gpu_lower" in
        *nvidia*)
            GPU_VENDOR="nvidia"
            GPU_MODEL="$HOST_GPU_INFO"
            ;;
        *amd*|*ati*|*advanced\ micro\ devices*)
            GPU_VENDOR="amd"
            GPU_MODEL="$HOST_GPU_INFO"
            ;;
        *intel*)
            GPU_VENDOR="intel"
            GPU_MODEL="$HOST_GPU_INFO"
            ;;
        *virtio*|*vmware*|*virtualbox*|*qxl*|*bochs*)
            GPU_VENDOR="virtual"
            GPU_MODEL="$HOST_GPU_INFO"
            ;;
        *)
            GPU_VENDOR="unknown"
            GPU_MODEL="$HOST_GPU_INFO"
            ;;
    esac

    if [[ "$GPU_VENDOR" == nvidia ]] && has_command nvidia-smi &&
        nvidia-smi -L >/dev/null 2>&1; then
        GPU_DRIVER_STATUS="installed"
    elif [[ "$GPU_DRIVER" != unavailable ]]; then
        GPU_DRIVER_STATUS="installed"
    elif [[ "$GPU_VENDOR" == unknown || "$GPU_VENDOR" == virtual ]]; then
        GPU_DRIVER_STATUS="unknown"
    else
        GPU_DRIVER_STATUS="missing"
    fi

    if [[ -n "$(env_value XDG_SESSION_TYPE)" ]]; then
        SESSION_STATUS="$(env_value XDG_SESSION_TYPE)"
    elif [[ -n "$(env_value WAYLAND_DISPLAY)" ]]; then
        SESSION_STATUS="wayland"
    elif [[ -n "$(env_value DISPLAY)" ]]; then
        SESSION_STATUS="x11"
    else
        SESSION_STATUS="console/unknown"
    fi
}

probe_vulkan() {
    local probe_pid
    local elapsed

    if has_command timeout; then
        timeout 8 vulkaninfo --summary >/dev/null 2>&1
        return $?
    fi

    # Some minimal installations do not ship coreutils' timeout. Keep the
    # installer bounded anyway so a broken ICD cannot block the whole setup.
    vulkaninfo --summary >/dev/null 2>&1 &
    probe_pid=$!
    elapsed=0
    while kill -0 "$probe_pid" >/dev/null 2>&1; do
        if ((elapsed >= 8)); then
            kill "$probe_pid" >/dev/null 2>&1 || true
            wait "$probe_pid" >/dev/null 2>&1 || true
            return 1
        fi
        sleep 1
        elapsed=$((elapsed + 1))
    done
    wait "$probe_pid"
}

probe_renderers() {
    VULKAN_STATUS="missing"
    if has_command vulkaninfo; then
        if probe_vulkan; then
            VULKAN_STATUS="available"
        else
            VULKAN_STATUS="failed"
        fi
    fi

    OPENGL_STATUS="unavailable"
    if has_command glxinfo && glxinfo -B >/dev/null 2>&1; then
        OPENGL_STATUS="available"
    elif has_command eglinfo && eglinfo >/dev/null 2>&1; then
        OPENGL_STATUS="available"
    elif has_command ldconfig && ldconfig -p 2>/dev/null | grep -q 'libEGL\.so\.1'; then
        OPENGL_STATUS="installed; display test unavailable"
    fi
}

append_package() {
    local package="$1"
    case " $INSTALL_PACKAGES " in
        *" $package "*) ;;
        *) INSTALL_PACKAGES="${INSTALL_PACKAGES:+$INSTALL_PACKAGES }$package" ;;
    esac
}

append_gpu_packages() {
    append_package ca-certificates
    append_package curl
    append_package pciutils
    append_package tar
    append_package mesa-utils
    append_package vulkan-tools

    case "$DISTRO_ID:$GPU_VENDOR" in
        ubuntu:nvidia|linuxmint:nvidia)
            # ubuntu-drivers chooses a matching package and is invoked later
            # only after an explicit confirmation.
            if (( ! SKIP_DRIVERS )); then
                append_package ubuntu-drivers-common
            fi
            ;;
        ubuntu:*|linuxmint:*)
            append_package linux-firmware
            ;;
        arch:intel)
            append_package vulkan-intel
            append_package linux-firmware
            ;;
        arch:amd)
            append_package vulkan-radeon
            append_package linux-firmware
            ;;
        arch:*)
            append_package linux-firmware
            ;;
    esac
}

package_is_installed() {
    local package="$1"
    case "$PACKAGE_MANAGER" in
        apt)
            dpkg-query -W -f='${db:Status-Status}' "$package" 2>/dev/null | grep -qx installed
            ;;
        pacman)
            pacman -Q "$package" >/dev/null 2>&1
            ;;
        *)
            return 1
            ;;
    esac
}

calculate_missing_packages() {
    local -a packages=()
    local -a missing=()
    local package

    MISSING_PACKAGES=""
    [[ -n "$INSTALL_PACKAGES" ]] || return 0
    IFS=' ' read -r -a packages <<< "$INSTALL_PACKAGES"
    for package in "${packages[@]}"; do
        if ! package_is_installed "$package"; then
            missing+=("$package")
        fi
    done
    if ((${#missing[@]} > 0)); then
        local previous_ifs="$IFS"
        IFS=' '
        MISSING_PACKAGES="${missing[*]}"
        IFS="$previous_ifs"
    fi
}

detect_distribution() {
    DISTRO_ID="$(read_os_release_value ID unknown | tr '[:upper:]' '[:lower:]')"
    DISTRO_LIKE="$(read_os_release_value ID_LIKE '')"
    DISTRO_NAME="$(read_os_release_value PRETTY_NAME "$DISTRO_ID")"
    DISTRO_VERSION="$(read_os_release_value VERSION_ID unknown)"
    DISTRO_SUPPORTED=0
    PACKAGE_MANAGER=""
    INSTALL_PACKAGES=""

    # Official support is intentionally narrow. Package-manager similarity is
    # not enough: seat, input and display-manager integration vary by distro.
    if [[ "$DISTRO_ID" == ubuntu || "$DISTRO_ID" == linuxmint ]]; then
        PACKAGE_MANAGER="apt"
        if has_command apt-get; then
            DISTRO_SUPPORTED=1
            if ((NO_BUILD)); then
                INSTALL_PACKAGES="libwayland-client0 libwayland-server0 libxkbcommon0 libegl1 libgl1 libvulkan1 libdrm2 libgbm1 libinput10 libseat1 seatd xwayland mesa-vulkan-drivers libgl1-mesa-dri flatpak dbus-user-session xdg-desktop-portal xdg-desktop-portal-gtk"
            else
                INSTALL_PACKAGES="build-essential pkg-config cargo rustc libwayland-dev libxkbcommon-dev libegl1-mesa-dev libgl1-mesa-dev libvulkan-dev libdrm-dev libgbm-dev libudev-dev libinput-dev libseat-dev wayland-protocols xwayland mesa-vulkan-drivers libgl1-mesa-dri flatpak seatd dbus-user-session xdg-desktop-portal xdg-desktop-portal-gtk"
            fi
        else
            warn "'$DISTRO_ID' é suportado, mas apt-get não foi encontrado"
        fi
    elif [[ "$DISTRO_ID" == arch ]]; then
        PACKAGE_MANAGER="pacman"
        if has_command pacman; then
            DISTRO_SUPPORTED=1
            if ((NO_BUILD)); then
                INSTALL_PACKAGES="wayland libxkbcommon libdrm libinput seatd xorg-xwayland vulkan-icd-loader vulkan-swrast mesa libglvnd flatpak xdg-desktop-portal xdg-desktop-portal-gtk dbus"
            else
                INSTALL_PACKAGES="base-devel rust cargo pkgconf wayland-protocols wayland libxkbcommon libdrm libinput seatd xorg-xwayland vulkan-headers vulkan-icd-loader vulkan-swrast mesa libglvnd flatpak xdg-desktop-portal xdg-desktop-portal-gtk dbus"
            fi
        else
            warn "Arch Linux é suportado, mas pacman não foi encontrado"
        fi
    else
        warn "distribuição não suportada oficialmente: $DISTRO_NAME (ID=$DISTRO_ID, ID_LIKE=$DISTRO_LIKE)"
    fi
}

show_host_report() {
    section "Diagnóstico do computador"
    printf 'Sistema: %s (%s)\n' "$DISTRO_NAME" "$HOST_ARCH"
    printf 'Versão: %s\n' "$DISTRO_VERSION"
    printf 'Kernel: %s\n' "$HOST_KERNEL"
    printf 'CPU: %s (%s threads)\n' "$HOST_CPU_MODEL" "$HOST_CPU_THREADS"
    if ((HOST_MEMORY_MIB > 0)); then
        printf 'Memória: %s MiB\n' "$HOST_MEMORY_MIB"
    else
        printf 'Memória: indisponível\n'
    fi
    printf 'GPU: %s\n' "$GPU_MODEL"
    printf 'Driver GPU: %s (%s)\n' "$GPU_DRIVER" "$GPU_DRIVER_STATUS"
    printf 'Sessão atual: %s\n' "$SESSION_STATUS"
    printf 'Vulkan: %s\n' "$VULKAN_STATUS"
    printf 'OpenGL: %s\n' "$OPENGL_STATUS"
    if ((HOST_MEMORY_MIB > 0 && HOST_MEMORY_MIB <= LOW_END_MEMORY_MIB)); then
        warn "memória limitada detectada; efeitos caros ficarão opcionais para priorizar fluidez"
    fi
    if [[ "$GPU_DRIVER_STATUS" == missing ]]; then
        warn "o driver da GPU parece ausente; a instalação tentará corrigir somente o caminho seguro da distro"
    fi
}

show_install_warning() {
    section "Antes de continuar"
    if [[ -n "$MISSING_PACKAGES" ]]; then
        info "pacotes que faltam: $MISSING_PACKAGES"
    else
        success "dependências principais já estão instaladas"
    fi
    warn "o instalador pode usar sudo/root para instalar pacotes e registrar a sessão Wayland"
    warn "nenhum driver proprietário será instalado sem uma confirmação explícita"
    warn "o display manager, a sessão atual e arquivos fora do Caelune não serão alterados"
    if [[ "$SESSION_STATUS" == wayland || "$SESSION_STATUS" == x11 ]]; then
        info "a sessão atual não será reiniciada; escolha Caelune no próximo login"
    fi
    confirm "Deseja continuar com a instalação do Caelune?"
}

setup_privilege() {
    if ((EUID == 0)); then
        PRIVILEGE_COMMAND=""
    elif has_command sudo; then
        PRIVILEGE_COMMAND="sudo"
    elif ((DRY_RUN)); then
        warn "sudo não foi encontrado; o dry-run mostrará comandos privilegiados, mas não os executará"
    else
        warn "sudo não foi encontrado; a instalação só poderá continuar se as dependências já estiverem instaladas e o destino for do usuário"
    fi
}

install_dependencies_legacy() {
    info "distribution: $DISTRO_ID"
    if [[ -n "$DISTRO_LIKE" ]]; then
        info "distribution family: $DISTRO_LIKE"
    fi
    info "package manager: $PACKAGE_MANAGER"
    info "packages: $INSTALL_PACKAGES"
    confirm "Install or update the minimum Rouch dependencies?"

    local -a packages=()
    IFS=' ' read -r -a packages <<< "$INSTALL_PACKAGES"
    case "$PACKAGE_MANAGER" in
        apt)
            run_privileged apt-get update
            run_privileged apt-get install -y "${packages[@]}"
            ;;
        pacman)
            run_privileged pacman -S --needed --noconfirm "${packages[@]}"
            ;;
        *)
            die "internal error: unsupported package manager '$PACKAGE_MANAGER'"
            ;;
    esac
}

install_dependencies() {
    [[ "$DISTRO_SUPPORTED" == 1 ]] ||
        die "esta distribuição não tem um fluxo de pacotes suportado; nenhum pacote foi alterado"

    calculate_missing_packages
    info "sistema: $DISTRO_NAME ($DISTRO_ID $DISTRO_VERSION)"
    info "gerenciador de pacotes: $PACKAGE_MANAGER"
    if [[ -z "$MISSING_PACKAGES" ]]; then
        success "dependências do Caelune já estão instaladas"
        return 0
    fi
    info "pacotes ausentes: $MISSING_PACKAGES"
    confirm "Instalar os pacotes ausentes agora?"

    local -a packages=()
    IFS=' ' read -r -a packages <<< "$MISSING_PACKAGES"
    case "$PACKAGE_MANAGER" in
        apt)
            run_privileged apt-get update
            run_privileged apt-get install -y "${packages[@]}"
            ;;
        pacman)
            run_privileged pacman -S --needed --noconfirm "${packages[@]}"
            ;;
        *)
            die "erro interno: gerenciador de pacotes não suportado '$PACKAGE_MANAGER'"
            ;;
    esac
    success "dependências instaladas"
}

repair_gpu_driver() {
    if ((SKIP_DRIVERS)); then
        warn "correção automática de driver desativada por --skip-drivers"
        return 0
    fi

    case "$GPU_VENDOR" in
        nvidia)
            if [[ "$GPU_DRIVER_STATUS" == installed ]]; then
                success "driver NVIDIA detectado: $GPU_DRIVER"
            elif [[ "$DISTRO_ID" == ubuntu || "$DISTRO_ID" == linuxmint ]]; then
                if ((DRY_RUN)); then
                    info "seria executado: ubuntu-drivers install (somente após confirmação)"
                elif ! has_command ubuntu-drivers; then
                    warn "ubuntu-drivers não está disponível; o sistema será instalado sem trocar o driver"
                else
                    warn "Ubuntu/Mint pode instalar um driver NVIDIA proprietário e pedir reinicialização"
                    confirm "Permitir que ubuntu-drivers escolha e instale o driver recomendado?"
                    if run_privileged ubuntu-drivers install; then
                        GPU_DRIVER_STATUS="installed"
                        success "driver NVIDIA instalado pelo mecanismo recomendado do Ubuntu"
                    else
                        warn "não foi possível instalar o driver NVIDIA automaticamente; o fallback gráfico continuará disponível"
                    fi
                fi
            else
                warn "GPU NVIDIA detectada, mas a instalação automática do driver não é segura nesta distro; consulte o gerenciador oficial de pacotes"
            fi
            ;;
        intel|amd)
            if [[ "$GPU_DRIVER_STATUS" == installed ]]; then
                success "driver de kernel detectado: $GPU_DRIVER"
            else
                warn "o driver de kernel de $GPU_VENDOR não foi confirmado; o instalador mantém Vulkan/OpenGL/software como fallback"
            fi
            ;;
        virtual)
            info "GPU virtual detectada; mantendo os drivers da sessão/hypervisor intactos"
            ;;
        *)
            warn "não foi possível identificar a GPU; nenhuma alteração de driver será forçada"
            ;;
    esac
}

configure_seat_access() {
    # libseat prefers logind and falls back to seatd. Installing the seatd
    # package is not enough: the socket has to be running and the account has
    # to be in the seat group. Without a usable seat the compositor cannot take
    # DRM master, and the display manager just returns to the login screen.
    local -a wanted_groups=(seat video input render)
    local -a missing_groups=()
    local group
    local seatd_unit=""
    local enable_seatd=0

    if ! has_command usermod || ! has_command getent || ! has_command id; then
        warn "usermod/getent/id não estão disponíveis; a checagem de seat foi ignorada"
        return 0
    fi

    for group in "${wanted_groups[@]}"; do
        getent group "$group" >/dev/null 2>&1 || continue
        if id -nG "$TARGET_USER" 2>/dev/null | tr ' ' '\n' | grep -qx "$group"; then
            continue
        fi
        missing_groups+=("$group")
    done

    if has_command systemctl; then
        for candidate in /usr/lib/systemd/system/seatd.service /lib/systemd/system/seatd.service; do
            [[ -e "$candidate" ]] || continue
            seatd_unit="$candidate"
            break
        done
        if [[ -n "$seatd_unit" ]] && ! systemctl is-enabled --quiet seatd 2>/dev/null; then
            enable_seatd=1
        fi
    fi

    if ((${#missing_groups[@]} == 0 && !enable_seatd)); then
        success "acesso a seat/DRM já está configurado para $TARGET_USER"
        return 0
    fi

    if ((${#missing_groups[@]} > 0)); then
        info "grupos que faltam para $TARGET_USER: ${missing_groups[*]}"
    fi
    if ((enable_seatd)); then
        info "seatd está instalado mas não está ativo; ele é o fallback quando o logind não atende"
    fi
    confirm "Configurar o acesso a seat/DRM (grupos e seatd) para $TARGET_USER?"

    for group in "${missing_groups[@]}"; do
        if ! run_privileged usermod -aG "$group" "$TARGET_USER"; then
            warn "não foi possível adicionar $TARGET_USER ao grupo $group"
        fi
    done
    if ((enable_seatd)); then
        if ! run_privileged systemctl enable --now seatd; then
            warn "não foi possível ativar o seatd; o logind continua sendo o caminho principal"
        fi
    fi
    if ((${#missing_groups[@]} > 0)); then
        info "os novos grupos só valem depois de sair e entrar de novo na sessão"
    fi
    success "acesso a seat/DRM configurado para $TARGET_USER"
}

configure_flatpak() {
    local remotes

    if ((SKIP_FLATHUB)); then
        warn "Flathub não foi configurado por causa de --skip-flatpak"
        return 0
    fi
    if ! has_command flatpak; then
        warn "Flatpak não está disponível; a galeria de aplicativos poderá ser configurada depois"
        return 0
    fi

    remotes="$(run_as_target flatpak remotes --user 2>/dev/null || true)"
    if printf '%s\n' "$remotes" | awk '$1 == "flathub" { found=1 } END { exit !found }'; then
        success "Flathub já está configurado para $TARGET_USER"
        return 0
    fi

    info "a galeria Caelune usa Flathub para descobrir aplicativos Flatpak"
    confirm "Adicionar o repositório oficial Flathub para $TARGET_USER?"
    if run_target flatpak remote-add --if-not-exists --user flathub \
        https://flathub.org/repo/flathub.flatpakrepo; then
        success "Flathub configurado para $TARGET_USER"
    else
        warn "não foi possível configurar Flathub agora; nenhum pacote Flatpak foi instalado"
    fi
}

install_target_file() {
    local source="$1"
    local destination="$2"
    local target_group

    if ((EUID == 0)) && [[ "$TARGET_USER" != root ]]; then
        target_group="$(id -gn "$TARGET_USER")"
        run install -o "$TARGET_USER" -g "$target_group" -m 0644 -- "$source" "$destination"
    else
        run_target install -m 0644 -- "$source" "$destination"
    fi
}

write_user_setting() {
    local key="$1"
    local value="$2"
    local target="$TARGET_CONFIG_DIR/$key"
    local stage="$WORK_DIR/config-$key"

    if [[ -e "$target" ]]; then
        info "preservando configuração existente: $target"
        return 0
    fi
    printf '%s\n' "$value" > "$stage"
    install_target_file "$stage" "$target"
}

configure_user_defaults() {
    local backend=0
    local reduce_transparency=false

    if [[ "$VULKAN_STATUS" != available ]]; then
        if [[ "$OPENGL_STATUS" == available || "$OPENGL_STATUS" == installed* ]]; then
            backend=1
        elif [[ "$VULKAN_STATUS" == failed && "$OPENGL_STATUS" == unavailable ]]; then
            backend=2
        fi
    fi
    if ((HOST_MEMORY_MIB > 0 && HOST_MEMORY_MIB <= LOW_END_MEMORY_MIB)); then
        reduce_transparency=true
    fi

    run_target mkdir -p -- "$TARGET_CONFIG_DIR" "$TARGET_STATE_DIR" "$TARGET_CACHE_DIR"
    write_user_setting "graphics.backend" "$backend"
    write_user_setting "reduce_transparency" "$reduce_transparency"
    write_user_setting "notifications.enabled" "true"
    write_user_setting "notifications.dnd" "false"
    write_user_setting "notifications.sounds" "true"
    write_user_setting "game_mode.mode" "auto"
    success "configuração inicial preservada/aplicada para $TARGET_USER"
}

setup_log() {
    LOG_FILE="$TARGET_STATE_DIR/$INSTALLER_LOG_NAME"
    run_target mkdir -p -- "$TARGET_STATE_DIR"
    run_target touch -- "$LOG_FILE"
    exec > >(tee -a "$LOG_FILE") 2>&1
    info "log desta instalação: $LOG_FILE"
}

choose_architecture() {
    local machine
    machine="$(uname -m)"
    case "$machine" in
        x86_64|amd64)
            printf 'x86_64\n'
            ;;
        aarch64|arm64)
            printf 'aarch64\n'
            ;;
        armv7l|armv7)
            printf 'armv7\n'
            ;;
        *)
            die "unsupported CPU architecture '$machine'; set ROUCH_BINARY_URL and add a release target"
            ;;
    esac
}

validate_https_url() {
    local url="$1"
    [[ "$url" == https://* ]] || die "refusing non-HTTPS download URL: $url"
}

download_file() {
    local url="$1"
    local destination="$2"

    validate_https_url "$url"
    info "downloading $url"
    if has_command curl; then
        run curl --fail --location --proto '=https' --tlsv1.2 --retry 3 \
            --connect-timeout 15 --output "$destination" "$url"
    elif has_command wget; then
        run wget --https-only --secure-protocol=TLSv1_2 --tries=3 \
            --timeout=20 --output-document="$destination" "$url"
    else
        die "curl or wget is required for downloads"
    fi
}

try_download_file() {
    local url="$1"
    local destination="$2"

    validate_https_url "$url"
    if has_command curl; then
        if curl --fail --silent --show-error --location \
            --proto '=https' --tlsv1.2 --retry 2 --connect-timeout 15 \
            --output "$destination" "$url"; then
            return 0
        fi
    elif has_command wget; then
        if wget --https-only --secure-protocol=TLSv1_2 --tries=2 \
            --timeout=20 --output-document="$destination" "$url"; then
            return 0
        fi
    fi
    rm -f -- "$destination"
    return 1
}

verify_sha256() {
    local file="$1"
    local expected="$2"
    [[ "$expected" =~ ^[[:xdigit:]]{64}$ ]] || die "invalid SHA-256 value"
    require_command sha256sum
    local actual
    actual="$(sha256sum "$file" | awk '{print $1}')"
    local normalized
    normalized="$(printf '%s' "$expected" | tr '[:upper:]' '[:lower:]')"
    [[ "$actual" == "$normalized" ]] || die "SHA-256 verification failed for $file"
    info "verified SHA-256 for $(basename "$file")"
}

release_download_url() {
    local repository="$1"
    local version="$2"
    local asset="$3"
    if [[ "$version" == "latest" ]]; then
        printf 'https://github.com/%s/releases/latest/download/%s\n' "$repository" "$asset"
    else
        printf 'https://github.com/%s/releases/download/%s/%s\n' "$repository" "$version" "$asset"
    fi
}

source_archive_url() {
    local repository="$1"
    local version="$2"
    if [[ "$version" == "latest" || "$version" == "main" ]]; then
        printf 'https://github.com/%s/archive/refs/heads/main.tar.gz\n' "$repository"
    else
        printf 'https://github.com/%s/archive/refs/tags/%s.tar.gz\n' "$repository" "$version"
    fi
}

safe_archive() {
    local archive="$1"
    local destination="$2"
    local member
    local archive_mode

    require_command tar
    tar -tzf "$archive" >/dev/null || die "could not read archive $archive"
    while IFS= read -r member; do
        [[ "$member" != /* ]] || die "archive contains an absolute path: $member"
        case "/$member/" in
            */../*)
                die "archive contains path traversal: $member"
                ;;
        esac
    done < <(tar -tzf "$archive")

    # Links can redirect a later write outside the staging directory.
    tar -tvzf "$archive" >/dev/null || die "could not inspect archive $archive"
    while IFS= read -r archive_mode; do
        case "$archive_mode" in
            l*|h*)
                die "archive contains a link entry; refusing extraction"
                ;;
        esac
    done < <(tar -tvzf "$archive")

    run mkdir -p -- "$destination"
    run tar --extract --gzip --file "$archive" --directory "$destination" \
        --no-same-owner --no-same-permissions --no-overwrite-dir
}

find_archive_root() {
    local extraction="$1"
    local candidate
    if [[ -f "$extraction/Cargo.toml" ]]; then
        printf '%s\n' "$extraction"
        return
    fi
    for candidate in "$extraction"/*; do
        if [[ -d "$candidate" && -f "$candidate/Cargo.toml" ]]; then
            printf '%s\n' "$candidate"
            return
        fi
    done
    die "source archive has no Cargo.toml"
}

find_interface_root() {
    local extraction="$1"
    local candidate
    if [[ -d "$extraction/interface-stage1" ]]; then
        printf '%s\n' "$extraction/interface-stage1"
        return
    fi
    for candidate in "$extraction"/*; do
        if [[ -d "$candidate" ]]; then
            printf '%s\n' "$candidate"
            return
        fi
    done
    printf '%s\n' "$extraction"
}

prepare_binary() {
    local architecture="$1"
    local repository="$2"
    local version="$3"
    local binary_stage="$WORK_DIR/rouch"

    if ((NO_BUILD)); then
        if [[ -n "$BINARY_PATH" ]]; then
            [[ -f "$BINARY_PATH" ]] || die "ROUCH_BINARY_PATH is not a regular file"
            run cp -- "$BINARY_PATH" "$binary_stage"
        else
            local binary_asset
            binary_asset="$(env_value ROUCH_BINARY_ASSET)"
            [[ -n "$binary_asset" ]] || binary_asset="$DEFAULT_BINARY_PREFIX-$architecture"
            local binary_url
            binary_url="$(env_value ROUCH_BINARY_URL)"
            [[ -n "$binary_url" ]] ||
                binary_url="$(release_download_url "$repository" "$version" "$binary_asset")"
            download_file "$binary_url" "$binary_stage"
            local binary_hash
            binary_hash="$(env_value ROUCH_BINARY_SHA256)"
            if [[ -n "$binary_hash" ]]; then
                verify_sha256 "$binary_stage" "$binary_hash"
            fi
        fi
    else
        local source_root
        if [[ -n "$SOURCE_DIR" ]]; then
            source_root="$SOURCE_DIR"
        else
            local script_path
            local script_root=""
            script_path="$0"
            if [[ -f "$script_path" ]]; then
                script_root="$(cd "$(dirname "$script_path")/.." && pwd)"
            fi
            if [[ -n "$script_root" && -f "$script_root/Cargo.toml" ]]; then
                source_root="$script_root"
            else
                local source_archive="$WORK_DIR/source.tar.gz"
                local source_url
                source_url="$(env_value ROUCH_SOURCE_URL)"
                [[ -n "$source_url" ]] || source_url="$(source_archive_url "$repository" "$version")"
                download_file "$source_url" "$source_archive"
                local source_hash
                source_hash="$(env_value ROUCH_SOURCE_SHA256)"
                if [[ -n "$source_hash" ]]; then
                    verify_sha256 "$source_archive" "$source_hash"
                fi
                safe_archive "$source_archive" "$WORK_DIR/source"
                source_root="$(find_archive_root "$WORK_DIR/source")"
            fi
        fi
        [[ -f "$source_root/Cargo.toml" ]] || die "source directory has no Cargo.toml: $source_root"
        require_command cargo
        info "building the Rouch compositor from source"
        run cargo build --locked --release --features native-session --manifest-path "$source_root/Cargo.toml"
        [[ -f "$source_root/target/release/rouch" ]] ||
            die "cargo build did not produce target/release/rouch"
        run cp -- "$source_root/target/release/rouch" "$binary_stage"
    fi
    [[ -s "$binary_stage" ]] || die "the staged Rouch binary is empty or missing"
    run chmod 0755 -- "$binary_stage"
}

prepare_interface_stage() {
    local repository="$1"
    local version="$2"
    local interface_archive="$WORK_DIR/interface-stage1.tar.gz"
    local interface_extraction="$WORK_DIR/interface-extracted"
    local source_root

    if [[ -n "$INTERFACE_DIR" ]]; then
        [[ -d "$INTERFACE_DIR" ]] || die "ROUCH_INTERFACE_DIR is not a directory"
        run cp -R -- "$INTERFACE_DIR" "$WORK_DIR/interface-stage1"
    else
        # A checkout invocation can stage the shipped visual assets without
        # requiring a release archive. Piped curl invocations do not have a
        # local script path, so they continue to use the signed/hashed asset
        # URL below.
        local script_path
        local script_root=""
        script_path="$0"
        if [[ -f "$script_path" ]]; then
            script_root="$(cd "$(dirname "$script_path")/.." && pwd)"
        fi
        if [[ -n "$script_root" && -f "$script_root/Wallpaper.webp" ]]; then
            run mkdir -p -- "$WORK_DIR/interface-stage1"
            run cp -- "$script_root/Wallpaper.webp" "$WORK_DIR/interface-stage1/"
            if [[ -f "$script_root/Icon-Composer.webp" ]]; then
                run cp -- "$script_root/Icon-Composer.webp" "$WORK_DIR/interface-stage1/"
            fi
        else
            local interface_asset
            local interface_url
            local interface_hash
            interface_asset="$(env_value ROUCH_INTERFACE_ASSET)"
            [[ -n "$interface_asset" ]] || interface_asset="$DEFAULT_INTERFACE_ASSET"
            interface_url="$(env_value ROUCH_INTERFACE_URL)"
            if [[ -n "$interface_url" ]]; then
                download_file "$interface_url" "$interface_archive"
            else
                interface_url="$(release_download_url "$repository" "$version" "$interface_asset")"
                if ! try_download_file "$interface_url" "$interface_archive"; then
                    warn "o asset visual separado não está publicado; usando os assets da interface presentes no código-fonte"
                    local source_archive="$WORK_DIR/source-interface.tar.gz"
                    local source_url
                    if [[ -f "$WORK_DIR/source.tar.gz" ]]; then
                        source_archive="$WORK_DIR/source.tar.gz"
                    else
                        source_url="$(source_archive_url "$repository" "$version")"
                        download_file "$source_url" "$source_archive"
                    fi
                    safe_archive "$source_archive" "$interface_extraction"
                    source_root="$(find_archive_root "$interface_extraction")"
                    run mkdir -p -- "$WORK_DIR/interface-stage1"
                    local copied_assets=0
                    local asset_name
                    local interface_asset_root="$source_root"
                    if [[ -d "$source_root/interface-stage1" ]]; then
                        interface_asset_root="$source_root/interface-stage1"
                    fi
                    for asset_name in Wallpaper.webp Icon-Composer.webp; do
                        if [[ -f "$interface_asset_root/$asset_name" ]]; then
                            run cp -- "$interface_asset_root/$asset_name" "$WORK_DIR/interface-stage1/"
                            copied_assets=1
                        fi
                    done
                    ((copied_assets)) ||
                        die "o repositório não contém os assets mínimos da primeira interface"
                fi
            fi
            if [[ -f "$interface_archive" ]]; then
                interface_hash="$(env_value ROUCH_INTERFACE_SHA256)"
                if [[ -n "$interface_hash" ]]; then
                    verify_sha256 "$interface_archive" "$interface_hash"
                fi
                safe_archive "$interface_archive" "$interface_extraction"
                source_root="$(find_interface_root "$interface_extraction")"
                run cp -R -- "$source_root" "$WORK_DIR/interface-stage1"
            fi
        fi
    fi

    [[ -d "$WORK_DIR/interface-stage1" ]] ||
        die "first-stage interface payload was not produced"
    run mkdir -p -- "$WORK_DIR/interface-stage1"
    printf 'stage=1\nactivated-by=rouch-installer\n' > "$WORK_DIR/interface-stage1/ROUCH-STAGE"
}

template_path() {
    local target="$1"
    [[ "$target" != *[[:space:]]* && "$target" != *"'"* ]] ||
        die "install path contains unsupported launcher characters"
    printf '%s\n' "$target"
}

write_launcher() {
    local launcher_stage="$1"
    local mode="$2"
    local binary_path
    local interface_path
    binary_path="$(template_path "$BINARY_TARGET")"
    interface_path="$(template_path "$INTERFACE_TARGET")"
    {
        printf '%s\n' '#!/bin/sh'
        printf '%s\n' 'set -eu'
        printf "binary='%s'\n" "$binary_path"
        printf '%s\n' "if [ \"\${1-}\" = \"--$mode\" ]; then"
        printf '%s\n' '    shift'
        printf '%s\n' 'fi'
        printf '%s\n' 'if [ ! -x "$binary" ]; then'
        printf '%s\n' '    printf "%s\\n" "Caelune launcher: executable not found: $binary" >&2'
        printf '%s\n' '    exit 127'
        printf '%s\n' 'fi'
        printf '%s\n' "export ROUCH_LAUNCH_MODE=$mode"
        printf "export ROUCH_INTERFACE_DIR='%s'\n" "$interface_path"
        if [[ "$mode" == session ]]; then
            printf '%s\n' 'export XDG_CURRENT_DESKTOP="${XDG_CURRENT_DESKTOP:-Caelune}"'
            printf '%s\n' 'export XDG_SESSION_DESKTOP="${XDG_SESSION_DESKTOP:-Caelune}"'
            printf '%s\n' 'export XDG_SESSION_TYPE="${XDG_SESSION_TYPE:-wayland}"'
        fi
        if [[ "$mode" == session ]]; then
            # The display manager truncates its own session log on every
            # attempt and returns to the greeter immediately, so a failed login
            # otherwise leaves no readable trace. Append to a durable log.
            printf '%s\n' 'log_file=""'
            printf '%s\n' 'state_dir="${XDG_STATE_HOME:-${HOME:-/tmp}/.local/state}/rouch"'
            printf '%s\n' 'if mkdir -p "$state_dir" 2>/dev/null && : >> "$state_dir/session.log" 2>/dev/null; then'
            printf '%s\n' '    log_file="$state_dir/session.log"'
            printf '%s\n' 'fi'
            printf '%s\n' 'if [ -n "$log_file" ]; then'
            printf '%s\n' '    printf "\\n=== %s: Caelune session start ===\\n" "$(date 2>/dev/null || printf "unknown time")" >> "$log_file" 2>/dev/null || true'
            printf '%s\n' '    exec "$binary" --session --no-fallback "$@" >> "$log_file" 2>&1'
            printf '%s\n' 'fi'
            printf '%s\n' 'exec "$binary" --session --no-fallback "$@"'
        else
            printf '%s\n' "exec \"\$binary\" --$mode \"\$@\""
        fi
    } > "$launcher_stage"
    chmod 0755 "$launcher_stage"
}

write_session_assets() {
    write_launcher "$WORK_DIR/rouch-session" session
    write_launcher "$WORK_DIR/rouch-nested" nested

    local session_exec
    session_exec="$(desktop_exec_value "$SESSION_LAUNCHER_TARGET")"
    {
        printf '%s\n' '[Desktop Entry]'
        printf '%s\n' 'Name=Caelune'
        printf '%s\n' 'Comment=Caelune Liquid Glass Wayland session'
        printf 'Exec=%s --session\n' "$session_exec"
        printf 'TryExec=%s\n' "$session_exec"
        printf '%s\n' 'Type=Application'
        printf '%s\n' 'DesktopNames=Caelune'
        printf '%s\n' 'X-Caelune-Launch-Mode=session'
    } > "$WORK_DIR/rouch-wayland-session.desktop"

    {
        printf '%s\n' '[Unit]'
        printf '%s\n' 'Description=Caelune Liquid Glass Wayland compositor (opt-in user service)'
        printf '%s\n' 'Documentation=https://github.com/AnThophicous/Caeluna'
        printf '%s\n' 'PartOf=graphical-session.target'
        printf '%s\n' 'After=graphical-session-pre.target'
        printf '%s\n' ''
        printf '%s\n' '[Service]'
        printf '%s\n' 'Type=simple'
        printf 'ExecStart=%s --session\n' "$SESSION_LAUNCHER_TARGET"
        printf '%s\n' 'Restart=on-failure'
        printf '%s\n' 'RestartSec=2s'
        printf '%s\n' 'OOMScoreAdjust=-100'
        printf '%s\n' 'Environment=RUST_LOG=rouch=info'
        printf '%s\n' ''
        printf '%s\n' '# Installed disabled: use the display manager session for seat/VT ownership.'
        printf '%s\n' ''
        printf '%s\n' '[Install]'
        printf '%s\n' 'WantedBy=graphical-session.target'
    } > "$WORK_DIR/rouch.service"
}

desktop_exec_value() {
    local target="$1"
    [[ "$target" != *$'\n'* && "$target" != *'"'* && "$target" != *'\'* ]] ||
        die "install path contains unsupported desktop-entry characters"
    printf '"%s"\n' "$target"
}

write_desktop_entry() {
    local desktop_stage="$WORK_DIR/rouch.desktop"
    local exec_value
    exec_value="$(desktop_exec_value "$NESTED_LAUNCHER_TARGET")"
    {
        printf '%s\n' '[Desktop Entry]'
        printf '%s\n' 'Type=Application'
        printf '%s\n' 'Version=1.0'
        printf '%s\n' 'Name=Caelune'
        printf '%s\n' 'GenericName=Liquid Glass Desktop'
        printf '%s\n' 'Comment=Launch the Caelune Liquid Glass desktop in nested mode'
        printf 'Exec=%s --nested\n' "$exec_value"
        printf 'TryExec=%s\n' "$exec_value"
        printf '%s\n' 'Icon=application-x-executable'
        printf '%s\n' 'Terminal=false'
        printf '%s\n' 'Categories=System;Utility;'
        printf '%s\n' 'Keywords=Caelune;Rouch;Wayland;Liquid;Glass;Desktop;'
        printf '%s\n' 'StartupNotify=true'
        printf '%s\n' 'X-Caelune-Launch-Mode=nested'
        printf '%s\n' 'X-Caelune-Interface-Stage=1'
    } > "$desktop_stage"
}

backup_file() {
    local target="$1"
    local backup="$2"
    local kind="$3"
    if [[ -L "$target" ]]; then
        die "refusing to replace symlink target: $target"
    fi
    if [[ -d "$target" ]]; then
        die "refusing to replace directory target: $target"
    fi
    if [[ -e "$target" ]]; then
        run_file cp -p -- "$target" "$backup"
        case "$kind" in
            binary) BINARY_BACKED_UP=1 ;;
            desktop) DESKTOP_BACKED_UP=1 ;;
            session-launcher) SESSION_LAUNCHER_BACKED_UP=1 ;;
            nested-launcher) NESTED_LAUNCHER_BACKED_UP=1 ;;
            session) SESSION_BACKED_UP=1 ;;
            user-service) USER_SERVICE_BACKED_UP=1 ;;
        esac
    fi
}

activate_files() {
    run_file mkdir -p -- "$BIN_DIR" "$DESKTOP_DIR" "$SHARE_DIR" "$SESSION_DIR" "$SYSTEMD_USER_DIR"
    if [[ -e "$BINARY_TARGET" || -L "$BINARY_TARGET" ]]; then
        confirm "Replace the existing Rouch binary at $BINARY_TARGET?"
    fi
    if [[ -e "$SESSION_LAUNCHER_TARGET" || -L "$SESSION_LAUNCHER_TARGET" ]]; then
        confirm "Replace the existing Rouch session launcher at $SESSION_LAUNCHER_TARGET?"
    fi
    if [[ -e "$NESTED_LAUNCHER_TARGET" || -L "$NESTED_LAUNCHER_TARGET" ]]; then
        confirm "Replace the existing Rouch nested launcher at $NESTED_LAUNCHER_TARGET?"
    fi
    if [[ -e "$DESKTOP_TARGET" || -L "$DESKTOP_TARGET" ]]; then
        confirm "Replace the existing Rouch desktop entry at $DESKTOP_TARGET?"
    fi
    if [[ -e "$SESSION_TARGET" || -L "$SESSION_TARGET" ]]; then
        confirm "Replace the existing Rouch Wayland session at $SESSION_TARGET?"
    fi
    if [[ -e "$USER_SERVICE_TARGET" || -L "$USER_SERVICE_TARGET" ]]; then
        confirm "Replace the existing Rouch user service at $USER_SERVICE_TARGET?"
    fi
    if [[ -e "$INTERFACE_TARGET" || -L "$INTERFACE_TARGET" ]]; then
        confirm "Replace the existing Rouch interface stage at $INTERFACE_TARGET?"
    fi

    BINARY_BACKUP="$WORK_DIR/old-binary"
    SESSION_LAUNCHER_BACKUP="$WORK_DIR/old-session-launcher"
    NESTED_LAUNCHER_BACKUP="$WORK_DIR/old-nested-launcher"
    DESKTOP_BACKUP="$WORK_DIR/old-desktop"
    SESSION_BACKUP="$WORK_DIR/old-session"
    USER_SERVICE_BACKUP="$WORK_DIR/old-user-service"
    INTERFACE_BACKUP="$WORK_DIR/old-interface-stage1"
    backup_file "$BINARY_TARGET" "$BINARY_BACKUP" binary
    backup_file "$SESSION_LAUNCHER_TARGET" "$SESSION_LAUNCHER_BACKUP" session-launcher
    backup_file "$NESTED_LAUNCHER_TARGET" "$NESTED_LAUNCHER_BACKUP" nested-launcher
    backup_file "$DESKTOP_TARGET" "$DESKTOP_BACKUP" desktop
    backup_file "$SESSION_TARGET" "$SESSION_BACKUP" session
    backup_file "$USER_SERVICE_TARGET" "$USER_SERVICE_BACKUP" user-service
    if [[ -L "$INTERFACE_TARGET" ]]; then
        die "refusing to replace symlink target: $INTERFACE_TARGET"
    fi
    if [[ -e "$INTERFACE_TARGET" ]]; then
        run_file mv -- "$INTERFACE_TARGET" "$INTERFACE_BACKUP"
        INTERFACE_BACKED_UP=1
    fi

    MUTATION_STARTED=1
    run_file install -m 0755 -- "$WORK_DIR/rouch" "$BINARY_TARGET"
    BINARY_INSTALLED=1
    run_file install -m 0755 -- "$WORK_DIR/rouch-session" "$SESSION_LAUNCHER_TARGET"
    SESSION_LAUNCHER_INSTALLED=1
    run_file install -m 0755 -- "$WORK_DIR/rouch-nested" "$NESTED_LAUNCHER_TARGET"
    NESTED_LAUNCHER_INSTALLED=1
    run_file install -m 0644 -- "$WORK_DIR/rouch.desktop" "$DESKTOP_TARGET"
    run_file install -m 0644 -- "$WORK_DIR/rouch-wayland-session.desktop" "$SESSION_TARGET"
    SESSION_INSTALLED=1
    run_file install -m 0644 -- "$WORK_DIR/rouch.service" "$USER_SERVICE_TARGET"
    USER_SERVICE_INSTALLED=1
    run_file mv -- "$WORK_DIR/interface-stage1" "$INTERFACE_TARGET"
    INTERFACE_INSTALLED=1
}

main_legacy() {
    local architecture
    local repository
    local version
    local user_home
    local custom_dir
    local temp_root
    local system_install=0

    detect_distribution
    setup_privilege
    repository="$(env_value ROUCH_GITHUB_REPO)"
    if [[ -z "$repository" ]]; then
        repository="$(env_value GITHUB_REPOSITORY)"
    fi
    [[ -n "$repository" ]] || repository="$DEFAULT_REPOSITORY"
    version="$(env_value ROUCH_VERSION)"
    [[ -n "$version" ]] || version="latest"
    architecture="$(choose_architecture)"

    if [[ -z "$INSTALL_PREFIX" ]]; then
        if ((EUID == 0)); then
            INSTALL_PREFIX="/usr/local"
        else
            user_home="$(env_value HOME)"
            [[ -n "$user_home" ]] || die "HOME is not set; pass ROUCH_PREFIX explicitly"
            INSTALL_PREFIX="$user_home/.local"
        fi
    fi
    [[ "$INSTALL_PREFIX" == /* ]] || die "ROUCH_PREFIX must be an absolute path"
    if ((EUID == 0)) || [[ "$INSTALL_PREFIX" == /usr/* ||
        "$INSTALL_PREFIX" == /opt/* || "$INSTALL_PREFIX" == /var/* ]]; then
        system_install=1
    fi

    BIN_DIR="$INSTALL_PREFIX/bin"
    custom_dir="$(env_value ROUCH_BIN_DIR)"
    [[ -z "$custom_dir" ]] || BIN_DIR="$custom_dir"
    SHARE_DIR="$INSTALL_PREFIX/share/rouch"
    custom_dir="$(env_value ROUCH_SHARE_DIR)"
    [[ -z "$custom_dir" ]] || SHARE_DIR="$custom_dir"
    if [[ -n "$DESKTOP_DIR_OVERRIDE" ]]; then
        DESKTOP_DIR="$DESKTOP_DIR_OVERRIDE"
    elif ((system_install)); then
        DESKTOP_DIR="/usr/share/applications"
    else
        user_home="$(env_value HOME)"
        [[ -n "$user_home" ]] || die "HOME is not set; pass ROUCH_DESKTOP_DIR explicitly"
        DESKTOP_DIR="$user_home/.local/share/applications"
    fi
    if [[ -n "$SESSION_DIR_OVERRIDE" ]]; then
        SESSION_DIR="$SESSION_DIR_OVERRIDE"
    elif ((system_install)); then
        SESSION_DIR="/usr/share/wayland-sessions"
    else
        user_home="$(env_value HOME)"
        [[ -n "$user_home" ]] || die "HOME is not set; pass ROUCH_SESSION_DIR explicitly"
        custom_dir="$(env_value XDG_DATA_HOME)"
        [[ -n "$custom_dir" ]] || custom_dir="$user_home/.local/share"
        SESSION_DIR="$custom_dir/wayland-sessions"
    fi
    if [[ -n "$SYSTEMD_USER_DIR_OVERRIDE" ]]; then
        SYSTEMD_USER_DIR="$SYSTEMD_USER_DIR_OVERRIDE"
    elif ((system_install)); then
        SYSTEMD_USER_DIR="/usr/lib/systemd/user"
    else
        user_home="$(env_value HOME)"
        [[ -n "$user_home" ]] || die "HOME is not set; pass ROUCH_SYSTEMD_USER_DIR explicitly"
        custom_dir="$(env_value XDG_CONFIG_HOME)"
        [[ -n "$custom_dir" ]] || custom_dir="$user_home/.config"
        SYSTEMD_USER_DIR="$custom_dir/systemd/user"
    fi
    [[ "$BIN_DIR" == /* && "$SHARE_DIR" == /* && "$DESKTOP_DIR" == /* &&
        "$SESSION_DIR" == /* && "$SYSTEMD_USER_DIR" == /* ]] ||
        die "install directories must be absolute"

    if ((EUID != 0)); then
        if ((system_install)) || [[ "$DESKTOP_DIR" == /usr/* ||
            "$DESKTOP_DIR" == /etc/* ||
            "$SESSION_DIR" == /usr/* ||
            "$SESSION_DIR" == /etc/* ||
            "$SYSTEMD_USER_DIR" == /usr/* ||
            "$SYSTEMD_USER_DIR" == /etc/* ||
            "$SYSTEMD_USER_DIR" == /var/* ]]; then
            FILE_PRIVILEGED=1
            [[ -n "$PRIVILEGE_COMMAND" ]] || die "system prefix requires sudo"
        fi
    fi

    BINARY_TARGET="$BIN_DIR/rouch"
    DESKTOP_TARGET="$DESKTOP_DIR/rouch.desktop"
    INTERFACE_TARGET="$SHARE_DIR/interface-stage1"
    SESSION_LAUNCHER_TARGET="$BIN_DIR/rouch-session"
    NESTED_LAUNCHER_TARGET="$BIN_DIR/rouch-nested"
    SESSION_TARGET="$SESSION_DIR/rouch.desktop"
    USER_SERVICE_TARGET="$SYSTEMD_USER_DIR/rouch.service"

    info "target prefix: $INSTALL_PREFIX"
    info "binary: $BINARY_TARGET"
    info "nested launcher: $NESTED_LAUNCHER_TARGET (--nested)"
    info "Wayland session: $SESSION_TARGET (--session)"
    info "user service (disabled): $USER_SERVICE_TARGET"
    info "interface: stage 1 only at $INTERFACE_TARGET"
    info "release: $repository @ $version ($architecture)"
    if ((NO_BUILD)); then
        info "build: disabled; use a prebuilt binary"
    else
        info "build: cargo --locked --release"
    fi

    if ((DRY_RUN)); then
        info "dry-run: no package, network, build, or filesystem changes will be made"
        install_dependencies
        if ((NO_BUILD)); then
            info "would download a prebuilt binary for $architecture"
        else
            info "would download source and build the release binary"
        fi
        info "would install the --nested launcher and register the --session Wayland entry"
        info "would install the optional disabled systemd user service"
        info "would download and activate only the first interface stage"
        info "dry-run complete"
        return 0
    fi

    install_dependencies
    require_command cp
    require_command chmod
    require_command install
    require_command mkdir
    require_command mv
    require_command rm
    temp_root="$(env_value TMPDIR)"
    [[ -n "$temp_root" ]] || temp_root="/tmp"
    WORK_DIR="$(mktemp -d "$temp_root/rouch-install.XXXXXXXX")"
    trap on_exit EXIT

    prepare_binary "$architecture" "$repository" "$version"
    prepare_interface_stage "$repository" "$version"
    write_session_assets
    write_desktop_entry
    activate_files
    info "Rouch installed successfully"
    if [[ ":$PATH:" != *":$BIN_DIR:"* ]]; then
        info "add $BIN_DIR to PATH to run 'rouch' from any terminal"
    fi
    info "the first interface stage is active; later stages remain untouched"
}

main() {
    local architecture
    local repository
    local version
    local custom_dir
    local temp_root
    local system_install=0

    detect_hardware
    detect_distribution
    if [[ "$DISTRO_SUPPORTED" == 1 ]]; then
        append_gpu_packages
    fi
    probe_renderers
    show_host_report

    if ((DIAGNOSE_ONLY)); then
        if [[ "$DISTRO_SUPPORTED" == 1 ]]; then
            calculate_missing_packages
            if [[ -n "$MISSING_PACKAGES" ]]; then
                info "pacotes ausentes detectados: $MISSING_PACKAGES"
            fi
        fi
        info "diagnóstico concluído; nenhum pacote, driver, arquivo ou sessão foi alterado"
        return 0
    fi
    [[ "$DISTRO_SUPPORTED" == 1 ]] ||
        die "instalação interrompida: use Linux Mint, Ubuntu ou Arch Linux oficialmente suportado"

    resolve_target_user
    setup_privilege
    repository="$(env_value CAELUNE_GITHUB_REPO)"
    [[ -n "$repository" ]] || repository="$(env_value ROUCH_GITHUB_REPO)"
    [[ -n "$repository" ]] || repository="$(env_value GITHUB_REPOSITORY)"
    [[ -n "$repository" ]] || repository="$DEFAULT_REPOSITORY"
    [[ "$repository" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] ||
        die "repositório inválido: $repository"
    version="$(env_value CAELUNE_VERSION)"
    [[ -n "$version" ]] || version="$(env_value ROUCH_VERSION)"
    [[ -n "$version" ]] || version="latest"
    [[ "$version" =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ ]] ||
        die "versão/tag inválida: $version"
    architecture="$(choose_architecture)"

    if [[ -z "$INSTALL_PREFIX" ]]; then
        if ((EUID == 0)); then
            INSTALL_PREFIX="/usr/local"
        else
            INSTALL_PREFIX="$TARGET_HOME/.local"
        fi
    fi
    [[ "$INSTALL_PREFIX" == /* ]] || die "CAELUNE_PREFIX/ROUCH_PREFIX must be an absolute path"
    if ((EUID == 0)) || [[ "$INSTALL_PREFIX" == /usr/* ||
        "$INSTALL_PREFIX" == /opt/* || "$INSTALL_PREFIX" == /var/* ]]; then
        system_install=1
    fi

    BIN_DIR="$INSTALL_PREFIX/bin"
    custom_dir="$(env_value ROUCH_BIN_DIR)"
    [[ -z "$custom_dir" ]] || BIN_DIR="$custom_dir"
    SHARE_DIR="$INSTALL_PREFIX/share/rouch"
    custom_dir="$(env_value ROUCH_SHARE_DIR)"
    [[ -z "$custom_dir" ]] || SHARE_DIR="$custom_dir"
    if [[ -n "$DESKTOP_DIR_OVERRIDE" ]]; then
        DESKTOP_DIR="$DESKTOP_DIR_OVERRIDE"
    elif ((system_install)); then
        DESKTOP_DIR="/usr/share/applications"
    else
        DESKTOP_DIR="$TARGET_HOME/.local/share/applications"
    fi
    if [[ -n "$SESSION_DIR_OVERRIDE" ]]; then
        SESSION_DIR="$SESSION_DIR_OVERRIDE"
    elif ((system_install)); then
        SESSION_DIR="/usr/share/wayland-sessions"
    else
        custom_dir="$(env_value XDG_DATA_HOME)"
        [[ -n "$custom_dir" ]] || custom_dir="$TARGET_HOME/.local/share"
        SESSION_DIR="$custom_dir/wayland-sessions"
    fi
    if [[ -n "$SYSTEMD_USER_DIR_OVERRIDE" ]]; then
        SYSTEMD_USER_DIR="$SYSTEMD_USER_DIR_OVERRIDE"
    elif ((system_install)); then
        SYSTEMD_USER_DIR="/usr/lib/systemd/user"
    else
        custom_dir="$(env_value XDG_CONFIG_HOME)"
        [[ -n "$custom_dir" ]] || custom_dir="$TARGET_HOME/.config"
        SYSTEMD_USER_DIR="$custom_dir/systemd/user"
    fi
    [[ "$BIN_DIR" == /* && "$SHARE_DIR" == /* && "$DESKTOP_DIR" == /* &&
        "$SESSION_DIR" == /* && "$SYSTEMD_USER_DIR" == /* ]] ||
        die "install directories must be absolute"

    if ((EUID != 0)); then
        if ((system_install)) || [[ "$DESKTOP_DIR" == /usr/* ||
            "$DESKTOP_DIR" == /etc/* ||
            "$SESSION_DIR" == /usr/* ||
            "$SESSION_DIR" == /etc/* ||
            "$SYSTEMD_USER_DIR" == /usr/* ||
            "$SYSTEMD_USER_DIR" == /etc/* ||
            "$SYSTEMD_USER_DIR" == /var/* ]]; then
            FILE_PRIVILEGED=1
            [[ -n "$PRIVILEGE_COMMAND" ]] || die "system prefix requires sudo"
        fi
    fi

    BINARY_TARGET="$BIN_DIR/rouch"
    DESKTOP_TARGET="$DESKTOP_DIR/rouch.desktop"
    INTERFACE_TARGET="$SHARE_DIR/interface-stage1"
    SESSION_LAUNCHER_TARGET="$BIN_DIR/rouch-session"
    NESTED_LAUNCHER_TARGET="$BIN_DIR/rouch-nested"
    SESSION_TARGET="$SESSION_DIR/rouch.desktop"
    USER_SERVICE_TARGET="$SYSTEMD_USER_DIR/rouch.service"

    calculate_missing_packages
    if ((EUID != 0 && !DRY_RUN)) && [[ -n "$MISSING_PACKAGES" && -z "$PRIVILEGE_COMMAND" ]]; then
        die "faltam dependências, mas sudo não está disponível; instale-as manualmente ou execute como root"
    fi
    section "Plano do Caelune"
    info "usuário da configuração: $TARGET_USER ($TARGET_HOME)"
    info "prefixo: $INSTALL_PREFIX"
    info "binário compatível: $BINARY_TARGET"
    info "launcher nested: $NESTED_LAUNCHER_TARGET (--nested)"
    info "sessão Wayland: $SESSION_TARGET (--session)"
    info "serviço systemd do usuário: $USER_SERVICE_TARGET (desativado)"
    info "interface: somente o estágio inicial em $INTERFACE_TARGET"
    info "release: $repository @ $version ($architecture)"
    if ((NO_BUILD)); then
        info "build: desativado; será usado um binário pré-compilado"
    else
        info "build: cargo --locked --release"
    fi
    show_install_warning

    if ((DRY_RUN)); then
        install_dependencies
        repair_gpu_driver
        info "seriam testados Vulkan e OpenGL novamente após instalar as dependências"
        info "seriam criadas configurações conservadoras em $TARGET_CONFIG_DIR"
        info "seria configurado o Flathub para $TARGET_USER, sem instalar aplicativos"
        if ((NO_BUILD)); then
            info "seria baixado um binário pré-compilado para $architecture"
        else
            info "seria baixado o código-fonte e compilado o binário de release"
        fi
        info "dry-run concluído; nenhuma mudança foi feita"
        return 0
    fi

    install_dependencies
    detect_hardware
    append_gpu_packages
    install_dependencies
    repair_gpu_driver
    detect_hardware
    probe_renderers
    show_host_report
    require_command cp
    require_command chmod
    require_command install
    require_command mkdir
    require_command mv
    require_command rm
    require_command mktemp
    require_command tee
    temp_root="$(env_value TMPDIR)"
    [[ -n "$temp_root" && "$temp_root" == /* ]] || temp_root="/tmp"
    WORK_DIR="$(mktemp -d "$temp_root/caelune-install.XXXXXXXX")"
    trap on_exit EXIT
    setup_log

    prepare_binary "$architecture" "$repository" "$version"
    prepare_interface_stage "$repository" "$version"
    write_session_assets
    write_desktop_entry
    activate_files
    configure_user_defaults
    configure_seat_access
    configure_flatpak
    success "Caelune instalado com sucesso"
    if [[ ":$PATH:" != *":$BIN_DIR:"* ]]; then
        info "adicione $BIN_DIR ao PATH para executar 'rouch' de qualquer terminal"
    fi
    info "a primeira interface está ativa; os estágios posteriores permanecem intocados"
    info "saia da sessão atual e escolha 'Caelune' no gerenciador de login"
}

main "$@"

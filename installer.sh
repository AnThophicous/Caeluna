#!/usr/bin/env bash
#
# Caelune installer entry point.
#
# The complete implementation lives in scripts/install.sh so the historical
# path remains valid. When this file is piped from GitHub, it downloads the
# matching implementation and executes it without requiring a checkout.

set -Eeuo pipefail
IFS=$'\n\t'

readonly DEFAULT_REPOSITORY="AnThophicous/Caeluna"
readonly SCRIPT_PATH="scripts/install.sh"

env_value() {
    printenv "$1" 2>/dev/null || true
}

banner() {
    printf '%s\n' \
        '   ____                 _                  ' \
        '  / ___|__ _  ___| | ___ _   _ _ __   ___ ' \
        ' | |   / _` |/ _ \ |/ / | | | | | | | / _ \' \
        ' | |__| (_| |  __/   <| |_| | | | | |  __/' \
        '  \____\__,_|\___|_|\_\__,_|_|_| |_\___|' \
        '' \
        '  Caelune — instalador do desktop Wayland Liquid Glass'
}

banner

source_ref="${BASH_SOURCE[0]}"
if [[ "$source_ref" == */* ]]; then
    source_dir="$(CDPATH= cd -- "$(dirname -- "$source_ref")" && pwd)"
else
    source_dir="$PWD"
fi
local_script="$source_dir/scripts/install.sh"
if [[ -f "$local_script" ]]; then
    export CAELUNE_BANNER_SHOWN=1
    exec bash "$local_script" "$@"
fi

repository="$(env_value CAELUNE_GITHUB_REPO)"
if [[ -z "$repository" ]]; then
    repository="$(env_value ROUCH_GITHUB_REPO)"
fi
if [[ -z "$repository" ]]; then
    repository="$DEFAULT_REPOSITORY"
fi

script_ref="$(env_value CAELUNE_INSTALLER_REF)"
if [[ -z "$script_ref" ]]; then
    script_ref="main"
fi
if [[ ! "$script_ref" =~ ^[A-Za-z0-9._/-]+$ || "$script_ref" == /* || "$script_ref" == *..* ]]; then
    printf 'Caelune installer: referência do instalador inválida: %s\n' "$script_ref" >&2
    exit 2
fi
version="$(env_value CAELUNE_VERSION)"
if [[ -z "$version" ]]; then
    version="$(env_value ROUCH_VERSION)"
fi
if [[ -z "$version" ]]; then
    version="latest"
fi

if [[ ! "$repository" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]]; then
    printf 'Caelune installer: repositório inválido: %s\n' "$repository" >&2
    exit 2
fi

source_url="https://raw.githubusercontent.com/$repository/$script_ref/$SCRIPT_PATH"
if command -v curl >/dev/null 2>&1; then
    curl --fail --location --proto '=https' --tlsv1.2 --retry 3 \
        --connect-timeout 15 "$source_url" | \
        CAELUNE_BANNER_SHOWN=1 CAELUNE_GITHUB_REPO="$repository" \
        CAELUNE_VERSION="$version" bash -s -- "$@"
    exit $?
fi

if command -v wget >/dev/null 2>&1; then
    wget --https-only --secure-protocol=TLSv1_2 --tries=3 \
        --timeout=20 -qO- "$source_url" | \
        CAELUNE_BANNER_SHOWN=1 CAELUNE_GITHUB_REPO="$repository" \
        CAELUNE_VERSION="$version" bash -s -- "$@"
    exit $?
fi

printf '%s\n' 'Caelune installer: curl ou wget é necessário quando o script é executado via GitHub.' >&2
exit 127
                                                            

#!/usr/bin/env sh
set -eu

REPO="sushanthpy/pipit"
BINARY="pipit"
CACHE_DIR="${HOME}/.pipit"
CACHE_FILE="${CACHE_DIR}/version-check"

# --- Uninstall -----------------------------------------------------------
uninstall() {
    existing="$(command -v "${BINARY}" 2>/dev/null || true)"
    if [ -z "${existing}" ]; then
        info "pipit is not installed (not found in PATH)"
        exit 0
    fi

    install_dir="$(dirname "${existing}")"

    info "Found pipit at ${existing}"

    if [ -w "${existing}" ]; then
        rm -f "${existing}"
    else
        info "Elevated permissions required to remove ${existing}"
        sudo rm -f "${existing}"
    fi

    info "Removed ${existing}"

    if [ -d "${CACHE_DIR}" ]; then
        rm -rf "${CACHE_DIR}"
        info "Removed cache directory ${CACHE_DIR}"
    fi

    config_dir="${HOME}/.config/pipit"
    if [ -d "${config_dir}" ]; then
        printf '\033[0;33mKeep config at %s? [Y/n] \033[0m' "${config_dir}"
        read -r keep_config </dev/tty 2>/dev/null || keep_config="y"
        case "${keep_config}" in
            [nN]*)
                rm -rf "${config_dir}"
                info "Removed ${config_dir}"
                ;;
            *)
                info "Kept ${config_dir}"
                ;;
        esac
    fi

    info "pipit has been uninstalled"
    exit 0
}

# Check for --uninstall / uninstall argument
for arg in "$@"; do
    case "${arg}" in
        --uninstall|uninstall)
            uninstall
            ;;
    esac
done

main() {
    platform="$(uname -s)"
    arch="$(uname -m)"

    case "${platform}" in
        Linux)  os="unknown-linux-gnu" ;;
        Darwin) os="apple-darwin" ;;
        *)      err "Unsupported platform: ${platform}" ;;
    esac

    case "${arch}" in
        x86_64|amd64)   arch="x86_64" ;;
        arm64|aarch64)   arch="aarch64" ;;
        *)               err "Unsupported architecture: ${arch}" ;;
    esac

    target="${arch}-${os}"

    if [ -n "${1:-}" ]; then
        tag="$1"
    else
        tag="$(get_latest_tag)"
    fi

    INSTALL_DIR="$(resolve_install_dir)"

    url="https://github.com/${REPO}/releases/download/${tag}/pipit-${tag}-${target}.tar.gz"

    info "Installing pipit ${tag} (${target})"
    info "Downloading ${url}"

    tmpdir="$(mktemp -d)"
    trap 'rm -rf "${tmpdir}"' EXIT

    if command -v curl >/dev/null 2>&1; then
        curl -fsSL "${url}" -o "${tmpdir}/pipit.tar.gz"
    elif command -v wget >/dev/null 2>&1; then
        wget -qO "${tmpdir}/pipit.tar.gz" "${url}"
    else
        err "Neither curl nor wget found. Install one and retry."
    fi

    tar xzf "${tmpdir}/pipit.tar.gz" -C "${tmpdir}"

    binary_path="$(find "${tmpdir}" -name "${BINARY}" -type f | head -1)"
    if [ -z "${binary_path}" ]; then
        err "Binary not found in archive"
    fi

    chmod +x "${binary_path}"

    if [ -w "${INSTALL_DIR}" ]; then
        mkdir -p "${INSTALL_DIR}"
        mv "${binary_path}" "${INSTALL_DIR}/${BINARY}"
    else
        info "Elevated permissions required to install to ${INSTALL_DIR}"
        sudo mkdir -p "${INSTALL_DIR}"
        sudo mv "${binary_path}" "${INSTALL_DIR}/${BINARY}"
    fi

    write_version_cache "${tag}"

    info "Installed pipit to ${INSTALL_DIR}/${BINARY}"
    "${INSTALL_DIR}/${BINARY}" --version

    active_path="$(command -v "${BINARY}" 2>/dev/null || true)"
    if [ -n "${active_path}" ] && [ "${active_path}" != "${INSTALL_DIR}/${BINARY}" ]; then
        warn "Your shell resolves ${BINARY} to ${active_path}, not ${INSTALL_DIR}/${BINARY}."
        warn "Either reorder PATH or rerun with PIPIT_INSTALL_DIR=$(dirname "${active_path}")"
    fi
}

resolve_install_dir() {
    if [ -n "${PIPIT_INSTALL_DIR:-}" ]; then
        printf '%s\n' "${PIPIT_INSTALL_DIR}"
        return
    fi

    existing="$(command -v "${BINARY}" 2>/dev/null || true)"
    if [ -n "${existing}" ]; then
        dirname "${existing}"
        return
    fi

    printf '%s\n' "/usr/local/bin"
}

get_latest_tag() {
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
            | grep '"tag_name"' | head -1 | sed 's/.*"tag_name": *"\([^"]*\)".*/\1/'
    elif command -v wget >/dev/null 2>&1; then
        wget -qO- "https://api.github.com/repos/${REPO}/releases/latest" \
            | grep '"tag_name"' | head -1 | sed 's/.*"tag_name": *"\([^"]*\)".*/\1/'
    else
        err "Neither curl nor wget found."
    fi
}

write_version_cache() {
    tag="$1"
    mkdir -p "${CACHE_DIR}" 2>/dev/null || return 0
    now="$(date +%s 2>/dev/null || echo 0)"
    printf '%s\n%s\n' "${now}" "${tag}" > "${CACHE_FILE}" 2>/dev/null || true
}

info() {
    printf '\033[0;32m=>\033[0m %s\n' "$1"
}

warn() {
    printf '\033[0;33mwarning:\033[0m %s\n' "$1"
}

err() {
    printf '\033[0;31merror:\033[0m %s\n' "$1" >&2
    exit 1
}

main "$@"

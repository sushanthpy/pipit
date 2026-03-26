#!/usr/bin/env sh
set -eu

REPO="sushanthpy/pipit"
INSTALL_DIR="${PIPIT_INSTALL_DIR:-/usr/local/bin}"
BINARY="pipit"

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
        mv "${binary_path}" "${INSTALL_DIR}/${BINARY}"
    else
        info "Elevated permissions required to install to ${INSTALL_DIR}"
        sudo mv "${binary_path}" "${INSTALL_DIR}/${BINARY}"
    fi

    info "Installed pipit to ${INSTALL_DIR}/${BINARY}"
    "${INSTALL_DIR}/${BINARY}" --version
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

info() {
    printf '\033[0;32m=>\033[0m %s\n' "$1"
}

err() {
    printf '\033[0;31merror:\033[0m %s\n' "$1" >&2
    exit 1
}

main "$@"

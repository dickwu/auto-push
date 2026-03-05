#!/usr/bin/env bash
set -euo pipefail

REPO="dickwu/auto-push"
INSTALL_DIR="${INSTALL_DIR:-/usr/local/bin}"

get_arch() {
    local arch
    arch="$(uname -m)"
    case "$arch" in
        x86_64|amd64) echo "x86_64" ;;
        aarch64|arm64) echo "aarch64" ;;
        *) echo "Unsupported architecture: $arch" >&2; exit 1 ;;
    esac
}

get_os() {
    local os
    os="$(uname -s)"
    case "$os" in
        Linux) echo "unknown-linux-gnu" ;;
        Darwin) echo "apple-darwin" ;;
        *) echo "Unsupported OS: $os" >&2; exit 1 ;;
    esac
}

main() {
    local arch os target version url tmpdir

    arch="$(get_arch)"
    os="$(get_os)"
    target="${arch}-${os}"

    if [ "${1:-}" = "--version" ] && [ -n "${2:-}" ]; then
        version="$2"
    else
        version="$(curl -sL "https://api.github.com/repos/${REPO}/releases/latest" | grep '"tag_name"' | head -1 | cut -d'"' -f4)"
    fi

    if [ -z "$version" ]; then
        echo "Error: could not determine latest version" >&2
        exit 1
    fi

    echo "Installing auto-push ${version} for ${target}..."

    url="https://github.com/${REPO}/releases/download/${version}/auto-push-${target}.tar.gz"
    tmpdir="$(mktemp -d)"
    trap 'rm -rf "$tmpdir"' EXIT

    curl -sL "$url" -o "${tmpdir}/auto-push.tar.gz"
    tar xzf "${tmpdir}/auto-push.tar.gz" -C "$tmpdir"

    if [ -w "$INSTALL_DIR" ]; then
        mv "${tmpdir}/auto-push" "${INSTALL_DIR}/auto-push"
    else
        echo "Installing to ${INSTALL_DIR} (requires sudo)..."
        sudo mv "${tmpdir}/auto-push" "${INSTALL_DIR}/auto-push"
    fi

    chmod +x "${INSTALL_DIR}/auto-push"
    echo "Installed auto-push to ${INSTALL_DIR}/auto-push"
    echo ""
    echo "Prerequisites:"
    echo "  - git:   https://git-scm.com"
    echo "  - gh:    https://cli.github.com"
    echo "  - claude: https://claude.ai/code"
}

main "$@"

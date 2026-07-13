#!/usr/bin/env bash
# The Lube Shop installer — builds and installs the `lubeshop` command.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
cd "$here"

echo "== The Lube Shop installer =="

# 1. Ensure a Rust toolchain is available (rustup installs to ~/.cargo, no sudo).
if ! command -v cargo >/dev/null 2>&1 && [ -f "$HOME/.cargo/env" ]; then
    # shellcheck disable=SC1091
    . "$HOME/.cargo/env"
fi
if ! command -v cargo >/dev/null 2>&1; then
    echo "Rust toolchain (cargo) not found."
    read -rp "Install rustup now? (user-space, no sudo) [y/N] " ans
    if [[ "${ans:-}" =~ ^[Yy] ]]; then
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
            | sh -s -- -y --profile minimal
        # shellcheck disable=SC1091
        . "$HOME/.cargo/env"
    else
        echo "Install Rust from https://rustup.rs and re-run." >&2
        exit 1
    fi
fi

# 2. Build (release) and install the binary to ~/.cargo/bin/lubeshop.
echo "Building and installing 'lubeshop' (this compiles in release mode)…"
cargo install --path crates/gwm-tui --locked --force

# 3. Report + PATH check.
bin="$(command -v lubeshop 2>/dev/null || echo "$HOME/.cargo/bin/lubeshop")"
echo
echo "Installed: $bin"
case ":$PATH:" in
    *":$HOME/.cargo/bin:"*) : ;;
    *) echo "NOTE: add ~/.cargo/bin to your PATH (rustup normally does this in ~/.bashrc)." ;;
esac
echo
echo "Run it with:   lubeshop"
echo "The external tools it drives (gw, cpmtools, mtools, VICE, amitools, …) can be"
echo "installed from the in-app Tools menu."

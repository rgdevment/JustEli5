#!/usr/bin/env bash
# Local CI: formatting, lints and tests for the Rust workspace.
set -euo pipefail
cd "$(dirname "$0")/.."

# whisper-rs needs libclang; on Windows bindgen only finds LLVM through this.
llvm="C:/Program Files/LLVM/bin"
if [ -z "${LIBCLANG_PATH:-}" ] && [ -f "$llvm/libclang.dll" ]; then export LIBCLANG_PATH="$llvm"; fi

cargo fmt --all -- --check
cargo clippy --workspace --all-targets --features mc-cli/whisper -- -D warnings
# Resolves the GPU feature wiring without needing the CUDA or Vulkan SDK.
cargo tree -p mc-cli --features mc-cli/cuda,mc-cli/vulkan -e features >/dev/null
cargo test --workspace --features mc-cli/whisper

# Native adapters only compile on their own OS; check them from here when the
# cross targets are installed (rustup target add <target>).
installed_targets=""
if command -v rustup >/dev/null; then
    installed_targets="$(rustup target list --installed)"
fi
for target in x86_64-pc-windows-gnu aarch64-apple-darwin; do
    if grep -qx "$target" <<<"$installed_targets"; then
        cargo clippy -p mc-captions -p mc-audio --target "$target" -- -D warnings
    else
        echo "skip: $target not installed"
    fi
done

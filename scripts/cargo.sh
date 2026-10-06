#!/bin/sh
# 使用 Workspace 工具链，不改 shell 配置；有现成 cargo 的机器也可直接 cargo。
set -eu
PROJECT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$PROJECT_DIR"
if command -v cargo >/dev/null 2>&1; then
    exec cargo "$@"
fi
RUST_WORKSPACE=/Users/xiedongjin/Workspace/tools/rust
export CARGO_HOME="$RUST_WORKSPACE/cargo"
export RUSTUP_HOME="$RUST_WORKSPACE/rustup"
export PATH="$CARGO_HOME/bin:$PATH"
exec "$CARGO_HOME/bin/cargo" "$@"

#!/usr/bin/env sh
set -eu

REPO_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
cd "$REPO_DIR"

cargo build --release
./target/release/rex-mdpeek install
echo "Installed ~/.local/bin/rex-mdpeek and reloaded Rex config."

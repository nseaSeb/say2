#!/usr/bin/env sh
# Build say2 in release mode, install the binary, and seed the bundled
# sentence deck. macOS only (say2 relies on the built-in `say` command).
set -e

BIN_DIR="${BIN_DIR:-/usr/local/bin}"
CONFIG="$HOME/.config/say2/sentences.toml"

echo "==> Building release binary…"
cargo build --release

echo "==> Installing binary to $BIN_DIR (may prompt for your password)…"
sudo cp target/release/say2 "$BIN_DIR/say2"

# Seed the bundled deck only on a fresh install — never clobber edits the
# user has already made to their config.
if [ -f "$CONFIG" ]; then
  echo "==> Keeping existing $CONFIG (not overwritten)."
else
  echo "==> Installing sentence deck to $CONFIG…"
  mkdir -p "$(dirname "$CONFIG")"
  cp sentences.toml "$CONFIG"
fi

echo "==> Done. Run: say2"

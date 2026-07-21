#!/usr/bin/env bash
#
# Usage:
#   git clone https://github.com/nlvm-lang/nlvm.git && cd nlvm && ./install.sh   (build from source)
#   curl -fsSL https://nlvm.dev/install.sh | bash                                (download prebuilt binary)

set -e

REPO="nlvm-lang/nlvm"
BIN_DIR="$HOME/.local/bin"

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1"
  else
    shasum -a 256 "$1"
  fi
}

# Running from inside a clone of the repo: build from source, same as always.
if [ -f "crates/nlc/Cargo.toml" ] && [ -f "crates/nlvm/Cargo.toml" ]; then
  PWD=$(pwd)
  cargo build -r
  mkdir -p "$BIN_DIR"
  ln -sf "$PWD/target/release/nlc" "$BIN_DIR/nlc"
  ln -sf "$PWD/target/release/nlvm" "$BIN_DIR/nlvm"
  echo "Built nlc and nlvm from source, linked into $BIN_DIR"
  exit 0
fi

# Otherwise (e.g. curl | bash): fetch a prebuilt binary from the latest GitHub release.
os=$(uname -s)
arch=$(uname -m)

case "$os-$arch" in
  Linux-x86_64) target="x86_64-unknown-linux-gnu" ;;
  Darwin-arm64) target="aarch64-apple-darwin" ;;
  *)
    echo "No prebuilt binary available for $os/$arch." >&2
    echo "Clone the repo and build from source instead:" >&2
    echo "  git clone https://github.com/$REPO.git && cd nlvm && ./install.sh" >&2
    exit 1
    ;;
esac

tag=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
  | grep -m1 '"tag_name"' | sed -E 's/.*"tag_name": *"([^"]+)".*/\1/')
if [ -z "$tag" ]; then
  echo "Could not determine the latest nlvm release." >&2
  exit 1
fi

pkg="nlvm-$tag-$target"
base_url="https://github.com/$REPO/releases/download/$tag"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

curl -fsSL -o "$tmp/$pkg.tar.gz" "$base_url/$pkg.tar.gz"

if curl -fsSL -o "$tmp/SHA256SUMS" "$base_url/SHA256SUMS" 2>/dev/null; then
  expected=$(grep "$pkg.tar.gz" "$tmp/SHA256SUMS" | cut -d' ' -f1)
  actual=$(sha256 "$tmp/$pkg.tar.gz" | cut -d' ' -f1)
  if [ -z "$expected" ] || [ "$expected" != "$actual" ]; then
    echo "Checksum verification failed for $pkg.tar.gz" >&2
    exit 1
  fi
else
  echo "Warning: no SHA256SUMS published for $tag, skipping checksum verification." >&2
fi

tar xzf "$tmp/$pkg.tar.gz" -C "$tmp"
mkdir -p "$BIN_DIR"
cp "$tmp/$pkg/nlc" "$tmp/$pkg/nlvm" "$BIN_DIR/"
chmod +x "$BIN_DIR/nlc" "$BIN_DIR/nlvm"

echo "Installed nlc and nlvm $tag into $BIN_DIR"
case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) echo "Note: $BIN_DIR is not on your \$PATH — add it to your shell profile." ;;
esac

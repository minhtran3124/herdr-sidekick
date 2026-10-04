#!/bin/sh
# Install-time build ([[build]]): fetch the prebuilt binary for this OS/arch from the GitHub
# release matching the manifest version, else build from source with cargo.
set -eu
cd "$(dirname "$0")/.."
# owner/name of the GitHub repo that hosts the releases. Empty until the repo is published;
# SIDEKICK_REPO overrides it (e.g. for a fork).
REPO=${SIDEKICK_REPO:-}
version=$(sed -n 's/^version = "\(.*\)"/\1/p' herdr-plugin.toml | head -n1)
case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) target=aarch64-apple-darwin ;;
  Darwin-x86_64) target=x86_64-apple-darwin ;;
  Linux-x86_64) target=x86_64-unknown-linux-musl ;;
  Linux-aarch64 | Linux-arm64) target=aarch64-unknown-linux-musl ;;
  *) target= ;;
esac

if [ -n "$REPO" ] && [ -n "$target" ] && command -v curl >/dev/null 2>&1; then
  url="https://github.com/$REPO/releases/download/v$version/sidekick-$target.tar.gz"
  tmp=$(mktemp -d)
  if curl -fsSL "$url" -o "$tmp/sidekick.tar.gz" && tar -xzf "$tmp/sidekick.tar.gz" -C "$tmp" && [ -x "$tmp/sidekick" ]; then
    mkdir -p bin
    mv "$tmp/sidekick" bin/sidekick
    rm -rf "$tmp"
    echo "sidekick $version for $target downloaded"
    exit 0
  fi
  rm -rf "$tmp"
  echo "no prebuilt binary at $url; building from source" >&2
fi

if command -v cargo >/dev/null 2>&1; then
  exec cargo build --release --locked
fi
echo "sidekick: no prebuilt binary for $(uname -s)-$(uname -m) and no cargo." >&2
echo "Install Rust (https://rustup.rs) and reinstall the plugin." >&2
exit 1

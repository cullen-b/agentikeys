#!/bin/sh
# Installs the latest agentikeys release for this machine, after checking its SHA-256 checksum.
#   curl -fsSL https://raw.githubusercontent.com/cullen-b/agentikeys/main/install.sh | sh
# Options (environment): AGENTIKEYS_VERSION=v0.1.0, AGENTIKEYS_INSTALL_DIR=/usr/local/bin
set -eu

repo="cullen-b/agentikeys"
dir="${AGENTIKEYS_INSTALL_DIR:-$HOME/.local/bin}"

case "$(uname -s)" in
  Darwin) os="apple-darwin" ;;
  Linux) os="unknown-linux-musl" ;;
  *) echo "agentikeys: unsupported OS $(uname -s); build from source with cargo" >&2; exit 1 ;;
esac
case "$(uname -m)" in
  arm64 | aarch64) arch="aarch64" ;;
  x86_64 | amd64) arch="x86_64" ;;
  *) echo "agentikeys: unsupported CPU $(uname -m); build from source with cargo" >&2; exit 1 ;;
esac

tag="${AGENTIKEYS_VERSION:-}"
if [ -z "$tag" ]; then
  tag="$(curl -fsSL "https://api.github.com/repos/$repo/releases/latest" | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n 1)"
fi
[ -n "$tag" ] || { echo "agentikeys: couldn't find the latest release" >&2; exit 1; }

name="agentikeys-$tag-$arch-$os"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
echo "Downloading $name"
curl -fsSL -o "$tmp/$name.tar.gz" "https://github.com/$repo/releases/download/$tag/$name.tar.gz"
curl -fsSL -o "$tmp/$name.tar.gz.sha256" "https://github.com/$repo/releases/download/$tag/$name.tar.gz.sha256"

if command -v shasum >/dev/null 2>&1; then
  (cd "$tmp" && shasum -a 256 -c "$name.tar.gz.sha256" >/dev/null)
else
  (cd "$tmp" && sha256sum -c "$name.tar.gz.sha256" >/dev/null)
fi || { echo "agentikeys: checksum mismatch, not installing" >&2; exit 1; }

tar xzf "$tmp/$name.tar.gz" -C "$tmp"
mkdir -p "$dir"
install -m 755 "$tmp/$name/agentikeys" "$dir/agentikeys"
echo "Installed agentikeys $tag to $dir/agentikeys"

case ":$PATH:" in
  *":$dir:"*) ;;
  *) echo "Add $dir to your PATH, e.g. echo 'export PATH=\"$dir:\$PATH\"' >> ~/.zshrc" ;;
esac
echo "Next: run \`agentikeys setup\` so the AI coding agents on this machine know how to use it."

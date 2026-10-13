#!/bin/sh
# Installs agentrelay from its GitHub releases.
#
#   curl -fsSL https://raw.githubusercontent.com/samehkhamis/agentrelay/main/install.sh | sh
#   ... | sh -s -- --version v0.1.0    a specific release
#   ... | sh -s -- --uninstall         remove it
#
# AGENTRELAY_INSTALL_DIR picks the folder (default ~/.local/bin).
set -eu

repo="samehkhamis/agentrelay"
dir="${AGENTRELAY_INSTALL_DIR:-$HOME/.local/bin}"
version="latest"

while [ $# -gt 0 ]; do
  case "$1" in
    --version)
      version="$2"
      shift 2
      ;;
    --uninstall)
      rm -f "$dir/agentrelay"
      echo "removed $dir/agentrelay (your data in ~/.agentrelay is kept)"
      exit 0
      ;;
    *)
      echo "unknown option: $1" >&2
      exit 1
      ;;
  esac
done

case "$(uname -s)" in
  Linux) os="unknown-linux-gnu" ;;
  Darwin) os="apple-darwin" ;;
  *) echo "unsupported OS: $(uname -s)" >&2; exit 1 ;;
esac
case "$(uname -m)" in
  x86_64 | amd64) arch="x86_64" ;;
  arm64 | aarch64) arch="aarch64" ;;
  *) echo "unsupported CPU: $(uname -m)" >&2; exit 1 ;;
esac

asset="agentrelay-$arch-$os.tar.gz"
if [ "$version" = latest ]; then
  url="https://github.com/$repo/releases/latest/download"
else
  url="https://github.com/$repo/releases/download/$version"
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
echo "downloading $asset ($version)"
curl -fsSL -o "$tmp/$asset" "$url/$asset"
curl -fsSL -o "$tmp/$asset.sha256" "$url/$asset.sha256"

# Check the download against its published checksum.
cd "$tmp"
if command -v sha256sum >/dev/null 2>&1; then
  check="sha256sum -c"
else
  check="shasum -a 256 -c"
fi
if ! $check "$asset.sha256" >/dev/null 2>&1; then
  echo "checksum mismatch for $asset; not installing" >&2
  exit 1
fi

tar -xzf "$asset"
mkdir -p "$dir"
install -m 755 agentrelay "$dir/agentrelay"
echo "installed $dir/agentrelay"

case ":$PATH:" in
  *":$dir:"*) ;;
  *) echo "$dir isn't on your PATH; add it with: echo 'export PATH=\"$dir:\$PATH\"' >> ~/.profile" ;;
esac

#!/bin/sh
# Install Sivro from GitHub releases on Linux or macOS.
#
#   curl -fsSL https://raw.githubusercontent.com/MohamedElashri/sivro/main/scripts/install.sh | sh
#
# SIVRO_VERSION=v0.1.0 pins a release (default: latest). An authenticated
# `gh` CLI is used when available, which also works for private repositories.
set -eu

repo=MohamedElashri/sivro
tag=${SIVRO_VERSION:-latest}
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
die() { echo "sivro install: $*" >&2; exit 1; }

if command -v gh >/dev/null 2>&1 && gh auth status >/dev/null 2>&1; then
  [ "$tag" = latest ] && tag=$(gh release view -R "$repo" --json tagName --jq .tagName)
  gh release view "$tag" -R "$repo" --json assets --jq '.assets[].name' > "$tmp/names"
  fetch() { gh release download "$tag" -R "$repo" -p "$1" -D "$tmp"; }
else
  command -v curl >/dev/null 2>&1 || die "curl is required"
  [ "$tag" = latest ] && path=latest || path="tags/$tag"
  curl -fsSL "https://api.github.com/repos/$repo/releases/$path" > "$tmp/release.json" ||
    die "cannot read release $tag (private repository? install and log in to gh)"
  tag=$(sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' "$tmp/release.json" | head -n 1)
  sed -n 's/.*"browser_download_url": *"[^"]*\/\([^"/]*\)".*/\1/p' "$tmp/release.json" > "$tmp/names"
  fetch() { curl -fsSL -o "$tmp/$1" "https://github.com/$repo/releases/download/$tag/$1"; }
fi

case "$(uname -s)" in
  Darwin) pattern='\.dmg$' ;;
  Linux)
    case "$(uname -m)" in
      x86_64 | amd64) arch='(x86_64|amd64|x64)' ;;
      aarch64 | arm64) arch='(aarch64|arm64)' ;;
      *) die "unsupported architecture $(uname -m)" ;;
    esac
    if command -v apt-get >/dev/null 2>&1; then ext='\.deb$'
    elif command -v dnf >/dev/null 2>&1 || command -v zypper >/dev/null 2>&1; then ext='\.rpm$'
    else ext='\.AppImage$'; fi
    pattern="$arch.*$ext" ;;
  *) die "unsupported system; download the package manually" ;;
esac

asset=$(grep -E "$pattern" "$tmp/names" | head -n 1) || true
[ -n "$asset" ] || die "release $tag has no package for this platform"
echo "Downloading $asset ($tag)"
fetch "$asset"
fetch SHA256SUMS.txt
if command -v sha256sum >/dev/null 2>&1; then sum='sha256sum -c -'; else sum='shasum -a 256 -c -'; fi
(cd "$tmp" && grep "  $asset\$" SHA256SUMS.txt | $sum) || die "checksum mismatch"

file="$tmp/$asset"
case "$asset" in
  *.dmg)
    hdiutil attach -nobrowse -quiet -mountpoint "$tmp/mnt" "$file"
    rm -rf /Applications/Sivro.app
    cp -R "$tmp/mnt/Sivro.app" /Applications/
    hdiutil detach -quiet "$tmp/mnt"
    # The app is not notarized; drop the download quarantine so it opens.
    xattr -dr com.apple.quarantine /Applications/Sivro.app 2>/dev/null || true
    echo "Installed /Applications/Sivro.app" ;;
  *.deb) sudo apt-get install -y "$file" ;;
  *.rpm)
    if command -v dnf >/dev/null 2>&1; then sudo dnf install -y "$file"
    else sudo zypper --non-interactive install --allow-unsigned-rpm "$file"; fi ;;
  *.AppImage)
    mkdir -p "$HOME/.local/bin"
    install -m 755 "$file" "$HOME/.local/bin/sivro"
    echo "Installed $HOME/.local/bin/sivro" ;;
esac

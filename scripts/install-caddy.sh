#!/usr/bin/env bash
# Installs the pinned Caddy release into a directory, after checking its SHA-256 against the
# value recorded here (each also matches the release's own SHA-512 checksums file). The
# release smoke runs the API behind it over HTTPS, as docs/OPERATING.md deploys it (D-074).
#
#   scripts/install-caddy.sh <directory>
set -euo pipefail

readonly VERSION="2.11.7"
dest="${1:?usage: scripts/install-caddy.sh <directory>}"

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64)  asset="caddy_${VERSION}_linux_amd64.tar.gz"; sha="727b91701a392de6ebc5027509f548bf39979e5216340d0faed8fa5e69c84f8b" ;;
  Linux-aarch64) asset="caddy_${VERSION}_linux_arm64.tar.gz"; sha="d8fc6d179a5d283028a472a5618564f6ad8a86fed513e64f032b3b0b7cc45e42" ;;
  Darwin-arm64)  asset="caddy_${VERSION}_mac_arm64.tar.gz";   sha="cda3030e5d5b13eb9f0b6fb541037f633bddd958c070c5f561d7a5cccb581665" ;;
  *) echo "error: no pinned Caddy build for $(uname -s)-$(uname -m)" >&2; exit 1 ;;
esac

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
curl -sSfL -o "$work/$asset" "https://github.com/caddyserver/caddy/releases/download/v$VERSION/$asset"
echo "$sha  $work/$asset" | shasum -a 256 -c - >/dev/null
tar -xzf "$work/$asset" -C "$work" caddy
mkdir -p "$dest"
install -m 0755 "$work/caddy" "$dest/caddy"
"$dest/caddy" version

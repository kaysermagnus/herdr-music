#!/usr/bin/env bash
# Install the prebuilt herdr-music binary for this platform.
#
# Downloads herdr-music-<target> and SHA256SUMS from the GitHub release
# tagged v<version> — never /latest — verifies the checksum, and places
# the binary at target/release/herdr-music, the path every command in
# herdr-plugin.toml runs.
#
# Version source: `version` in herdr-plugin.toml (kept in sync with the
# crate version in Cargo.toml). If the download or checksum fails and
# cargo is on PATH, falls back to `cargo build --release`; otherwise
# exits non-zero.
set -euo pipefail

repo="kaysermagnus/herdr-music"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

version="$(sed -n 's/^version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' herdr-plugin.toml | head -n 1)"
if [ -z "$version" ]; then
  echo "fetch-binary: could not read version from herdr-plugin.toml" >&2
  exit 1
fi

# The asset names produced by .github/workflows/release.yml.
case "$(uname -s):$(uname -m)" in
  Linux:x86_64)  target="x86_64-unknown-linux-gnu" ;;
  Darwin:x86_64) target="x86_64-apple-darwin" ;;
  Darwin:arm64)  target="aarch64-apple-darwin" ;;
  *)             target="" ;;
esac

bin="herdr-music-$target"
tag="v$version"
# Overridable for testing (e.g. a file:// mirror of a release directory).
base="${HERDR_MUSIC_BASE_URL:-https://github.com/$repo/releases/download/$tag}"
if [ -n "${HERDR_MUSIC_BASE_URL:-}" ]; then
  echo "fetch-binary: HERDR_MUSIC_BASE_URL override active: $base" >&2
fi
dest="target/release/herdr-music"

fetch() {
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$1" -o "$2"
  elif command -v wget >/dev/null 2>&1; then
    wget -q "$1" -O "$2"
  else
    return 1
  fi
}

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

build_from_source() {
  if command -v cargo >/dev/null 2>&1; then
    echo "fetch-binary: falling back to cargo build --release" >&2
    cargo build --release
  else
    echo "fetch-binary: no prebuilt binary available and cargo is not on PATH" >&2
    exit 1
  fi
}

if [ -n "$target" ]; then
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT
  ok=0
  if fetch "$base/$bin" "$tmp/$bin"; then
    if fetch "$base/SHA256SUMS" "$tmp/SHA256SUMS"; then
      expected="$(awk -v f="$bin" '{name=$NF; sub(/^\*/, "", name)} name == f {print $1}' "$tmp/SHA256SUMS")"
      if [ -n "$expected" ] && [ "$(sha256 "$tmp/$bin")" = "$expected" ]; then
        mkdir -p target/release
        cp "$tmp/$bin" "$dest"
        chmod +x "$dest"
        echo "fetch-binary: installed $bin ($tag) -> $dest"
        ok=1
      else
        echo "fetch-binary: sha256 verification failed for $bin" >&2
      fi
    else
      echo "fetch-binary: no SHA256SUMS asset at tag $tag" >&2
    fi
  else
    echo "fetch-binary: no release asset $bin at tag $tag" >&2
  fi
  if [ "$ok" -ne 1 ]; then
    build_from_source
  fi
else
  echo "fetch-binary: unsupported platform $(uname -s) $(uname -m)" >&2
  build_from_source
fi

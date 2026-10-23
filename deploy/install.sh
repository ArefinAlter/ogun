#!/bin/sh
# Installs Ogun release binaries for this machine into ~/.local/bin (or --prefix), after checking each archive
# against the release's SHA256SUMS. Linux and macOS; on Windows, download the .zip from the release page.
#
#   curl -fsSL https://raw.githubusercontent.com/ArefinAlter/ogun/main/deploy/install.sh | sh
#   sh install.sh --version v0.1.0 --prefix "$HOME/.local" --libc gnu
#
# Environment: OGUN_VERSION, OGUN_PREFIX, OGUN_LIBC (musl or gnu, Linux only), OGUN_REPO.

set -eu

repo="${OGUN_REPO:-ArefinAlter/ogun}"
version="${OGUN_VERSION:-latest}"
prefix="${OGUN_PREFIX:-$HOME/.local}"
libc="${OGUN_LIBC:-musl}"

say() { printf '%s\n' "$*"; }
die() { printf 'install.sh: %s\n' "$*" >&2; exit 1; }

while [ $# -gt 0 ]; do
  case "$1" in
    --version) version="${2:?--version needs a value}"; shift 2 ;;
    --prefix) prefix="${2:?--prefix needs a value}"; shift 2 ;;
    --libc) libc="${2:?--libc needs a value}"; shift 2 ;;
    --repo) repo="${2:?--repo needs a value}"; shift 2 ;;
    -h|--help)
      sed -n '2,10p' "$0" | sed 's/^# \{0,1\}//'
      exit 0 ;;
    *) die "unknown option $1" ;;
  esac
done

case "$libc" in musl|gnu) ;; *) die "--libc must be musl or gnu" ;; esac

if command -v curl >/dev/null 2>&1; then
  fetch() { curl -fsSL --proto '=https' --tlsv1.2 -o "$2" "$1"; }
  resolve_latest() { curl -fsSL --proto '=https' --tlsv1.2 -o /dev/null -w '%{url_effective}' "https://github.com/$repo/releases/latest"; }
elif command -v wget >/dev/null 2>&1; then
  fetch() { wget -q --https-only -O "$2" "$1"; }
  resolve_latest() { wget -q --https-only --max-redirect=5 -S -O /dev/null "https://github.com/$repo/releases/latest" 2>&1 | sed -n 's/^ *[Ll]ocation: *//p' | tail -n 1 | tr -d '\r'; }
else
  die "curl or wget is required"
fi

if command -v sha256sum >/dev/null 2>&1; then
  sha256() { sha256sum "$1" | cut -d ' ' -f 1; }
elif command -v shasum >/dev/null 2>&1; then
  sha256() { shasum -a 256 "$1" | cut -d ' ' -f 1; }
else
  die "sha256sum or shasum is required to verify the download"
fi

os="$(uname -s)"
arch="$(uname -m)"
case "$arch" in
  x86_64|amd64) arch=x86_64 ;;
  aarch64|arm64) arch=aarch64 ;;
  *) die "no release build for architecture $arch" ;;
esac
case "$os" in
  Linux) target="$arch-unknown-linux-$libc" ;;
  Darwin) target="$arch-apple-darwin" ;;
  *) die "no release build for $os; on Windows use the .zip from the release page" ;;
esac

if [ "$version" = "latest" ]; then
  url="$(resolve_latest)" || die "cannot find the latest release of $repo"
  version="${url##*/}"
  case "$version" in v*) ;; *) die "cannot find the latest release of $repo" ;; esac
fi

name="ogun-$version-$target"
archive="$name.tar.gz"
base="https://github.com/$repo/releases/download/$version"

tmp="$(mktemp -d 2>/dev/null || mktemp -d -t ogun)"
trap 'rm -rf "$tmp"' EXIT INT TERM

say "Downloading $archive ($version)"
fetch "$base/$archive" "$tmp/$archive" || die "cannot download $base/$archive"
fetch "$base/SHA256SUMS" "$tmp/SHA256SUMS" || die "cannot download $base/SHA256SUMS"

expected="$(awk -v f="$archive" '{ n = $2; sub(/^\*/, "", n); if (n == f) print $1 }' "$tmp/SHA256SUMS")"
[ -n "$expected" ] || die "SHA256SUMS has no entry for $archive"
actual="$(sha256 "$tmp/$archive")"
[ "$expected" = "$actual" ] || die "checksum mismatch for $archive (expected $expected, got $actual); nothing was installed"
say "Checksum verified"

tar -xzf "$tmp/$archive" -C "$tmp"
[ -d "$tmp/$name" ] || die "unexpected archive layout"

mkdir -p "$prefix/bin"
installed=""
for bin in ogun ogund ogun-ingest ogun-server; do
  if [ -f "$tmp/$name/$bin" ]; then
    # Install via a temporary name, then rename, so a running binary is never half-written.
    cp "$tmp/$name/$bin" "$prefix/bin/.$bin.new"
    chmod 0755 "$prefix/bin/.$bin.new"
    mv -f "$prefix/bin/.$bin.new" "$prefix/bin/$bin"
    installed="$installed $bin"
  fi
done
[ -n "$installed" ] || die "the archive holds no Ogun binaries"

say "Installed$installed into $prefix/bin"
case ":$PATH:" in
  *":$prefix/bin:"*) ;;
  *) say "Add $prefix/bin to your PATH, for example: export PATH=\"$prefix/bin:\$PATH\"" ;;
esac

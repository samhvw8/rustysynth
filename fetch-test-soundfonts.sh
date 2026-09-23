#!/usr/bin/env bash
# Downloads the soundfonts the test suite expects at the repository root and checks their SHA-256.
# TimGM6mb (GPL-2) comes from Debian's timgm6mb-soundfont package; GeneralUser GS MuseScore v1.442
# (free to use and redistribute, see its license) from a public mirror of the MuseScore 2 bundle.
set -euo pipefail
cd "$(dirname "$0")"

sha256() { if command -v sha256sum >/dev/null; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -d' ' -f1; }

check() {
  local file=$1 want=$2 got
  got=$(sha256 "$file")
  [[ "$got" == "$want" ]] || { echo "$file: sha256 $got, expected $want" >&2; rm -f "$file"; exit 1; }
  echo "ok  $file"
}

if [[ ! -f TimGM6mb.sf2 ]]; then
  tmp=$(mktemp -d)
  curl -fsSL -o "$tmp/tim.deb" https://deb.debian.org/debian/pool/main/t/timgm6mb-soundfont/timgm6mb-soundfont_1.3-5_all.deb
  (cd "$tmp" && ar x tim.deb && tar xf data.tar.*)
  cp "$tmp/usr/share/sounds/sf2/TimGM6mb.sf2" TimGM6mb.sf2
  rm -rf "$tmp"
fi
check TimGM6mb.sf2 c5378b62028c920cb11e4803327983fee2f2cdff5dc89c708e39da417e51c854

gu="GeneralUser GS MuseScore v1.442.sf2"
if [[ ! -f "$gu" ]]; then
  curl -fsSL -o "$gu" "https://raw.githubusercontent.com/bradhowes/SynthInC/master/Resources/SoundFonts/GeneralUser%20GS%20MuseScore%20v1.442.sf2"
fi
check "$gu" d910e139f619048b331d72d2e0867f1729f9725e2a97a6be32b09b8b5b5c4b12

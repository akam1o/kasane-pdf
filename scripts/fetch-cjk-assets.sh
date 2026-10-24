#!/usr/bin/env bash
# Fetch the licence-clean assets the `cjk-fallback` feature embeds into
# assets/cjk/ (committed; this script is how they are updated and how
# the committed copy is verified — `--check`):
#   * Adobe-Japan1 predefined CMaps  — adobe-type-tools/cmap-resources (BSD-3-Clause)
#   * Adobe-Japan1-UCS2 CID→Unicode  — adobe-type-tools/mapping-resources-pdf (BSD-3-Clause)
#   * Noto Sans JP Regular (JP subset OTF) — notofonts/noto-cjk (SIL OFL 1.1)
#   * the licence file of each source
#
# Reproducible: every file is fetched from a pinned upstream commit and
# checked against scripts/cjk-assets.sha256 before it is installed; a
# download failure, a missing licence or a checksum mismatch fails the
# script and leaves assets/cjk/ untouched (nothing partial is ever
# installed).
#
# Updating: bump the commit pins below, run
#   scripts/fetch-cjk-assets.sh --update-lock
# review the new scripts/cjk-assets.sha256 (and the licence files), then
# commit the pins, the lock and assets/cjk/ together. See
# docs/third-party.md. `scripts/fetch-cjk-assets.sh --check` verifies the
# committed assets against the lock (CI runs it; no network).
set -euo pipefail
cd "$(dirname "$0")/.."

# ---- pinned upstream commits ---------------------------------------
CMAP_REV=f5cf3bca7fdfeaceb77aa82847e974f2306c20b4   # adobe-type-tools/cmap-resources, 2023-11-15
MAP_REV=2dd5e53fb74a01718b9dfd448a0d1cce6fff2aa5    # adobe-type-tools/mapping-resources-pdf, 2023-01-17
NOTO_REV=f8d157532fbfaeda587e826d4cd5b21a49186f7c   # notofonts/noto-cjk, 2024-09-19

cmap_base="https://raw.githubusercontent.com/adobe-type-tools/cmap-resources/$CMAP_REV"
map_base="https://raw.githubusercontent.com/adobe-type-tools/mapping-resources-pdf/$MAP_REV"
noto_base="https://raw.githubusercontent.com/notofonts/noto-cjk/$NOTO_REV"

lock=scripts/cjk-assets.sha256
dst=assets/cjk
update=0
[ "${1:-}" = "--update-lock" ] && update=1

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
  else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

# --check: verify the assets already present in assets/cjk/ (they are
# committed) against the lock without touching the network.
if [ "${1:-}" = "--check" ]; then
  bad=0
  while read -r sum rel; do
    [ -n "$rel" ] || continue
    if [ ! -f "$dst/$rel" ]; then echo "error: $dst/$rel missing" >&2; bad=1; continue; fi
    got=$(sha256_of "$dst/$rel")
    [ "$got" = "$sum" ] || { echo "error: checksum mismatch for $rel" >&2; bad=1; }
  done < "$lock"
  n=$(find "$dst" -type f | wc -l | tr -d ' ')
  [ "$n" = "$(grep -c . "$lock")" ] || { echo "error: $dst has $n files, lock lists $(grep -c . "$lock")" >&2; bad=1; }
  [ "$bad" = 0 ] && echo "assets/cjk: $n files match $lock" && exit 0
  exit 1
fi

# ---- file list: "<relative path in assets/cjk>|<url>" -----------------
files=()
for n in H V 90ms-RKSJ-H 90ms-RKSJ-V 90msp-RKSJ-H 90msp-RKSJ-V 90pv-RKSJ-H 90pv-RKSJ-V \
         83pv-RKSJ-H Add-RKSJ-H Add-RKSJ-V Ext-RKSJ-H Ext-RKSJ-V EUC-H EUC-V \
         UniJIS-UCS2-H UniJIS-UCS2-V UniJIS-UCS2-HW-H UniJIS-UCS2-HW-V \
         UniJIS-UTF16-H UniJIS-UTF16-V UniJIS2004-UTF16-H UniJIS2004-UTF16-V \
         UniJISX0213-UTF32-H UniJISX0213-UTF32-V UniJISX02132004-UTF32-H UniJISX02132004-UTF32-V; do
  files+=("cmap/$n|$cmap_base/Adobe-Japan1-7/CMap/$n")
done
files+=("cmap/LICENSE|$cmap_base/LICENSE.md")
files+=("cmap/Adobe-Japan1-UCS2|$map_base/pdf2unicode/Adobe-Japan1-UCS2")
files+=("cmap/Adobe-Japan1-UCS2-LICENSE|$map_base/LICENSE.txt")
files+=("NotoSansJP-Regular.otf|$noto_base/Sans/SubsetOTF/JP/NotoSansJP-Regular.otf")
files+=("NotoSansJP-LICENSE|$noto_base/Sans/LICENSE")

tmp=$(mktemp -d "${TMPDIR:-/tmp}/kasane-cjk.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/cmap"

for spec in "${files[@]}"; do
  rel=${spec%%|*}; url=${spec#*|}
  echo "fetch $url"
  if ! curl -fsSL --retry 3 --retry-all-errors -o "$tmp/$rel" "$url"; then
    echo "error: download failed: $url" >&2; exit 1
  fi
  [ -s "$tmp/$rel" ] || { echo "error: empty download: $url" >&2; exit 1; }
done

if [ "$update" = 1 ]; then
  : > "$lock.tmp"
  for spec in "${files[@]}"; do
    rel=${spec%%|*}
    printf '%s  %s\n' "$(sha256_of "$tmp/$rel")" "$rel" >> "$lock.tmp"
  done
  mv "$lock.tmp" "$lock"
  echo "wrote $lock ($(wc -l < "$lock" | tr -d ' ') files)"
fi

[ -f "$lock" ] || { echo "error: $lock missing (run with --update-lock after reviewing the pins)" >&2; exit 1; }
bad=0
while read -r sum rel; do
  [ -n "$rel" ] || continue
  if [ ! -f "$tmp/$rel" ]; then echo "error: $rel listed in $lock but not fetched" >&2; bad=1; continue; fi
  got=$(sha256_of "$tmp/$rel")
  if [ "$got" != "$sum" ]; then echo "error: checksum mismatch for $rel: expected $sum, got $got" >&2; bad=1; fi
done < "$lock"
for spec in "${files[@]}"; do
  rel=${spec%%|*}
  grep -q " $rel\$" "$lock" || { echo "error: $rel fetched but not listed in $lock" >&2; bad=1; }
done
[ "$bad" = 0 ] || { echo "error: assets not installed" >&2; exit 1; }

rm -rf "$dst"
mkdir -p "$(dirname "$dst")"
mv "$tmp" "$dst"
trap - EXIT
echo "installed $dst ($(wc -l < "$lock" | tr -d ' ') files verified against $lock)"

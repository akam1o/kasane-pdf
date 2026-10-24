#!/usr/bin/env bash
# Fetch the licence-clean assets the `cjk-fallback` feature embeds:
#   * Adobe-Japan1 predefined CMaps (Adobe cmap-resources, BSD-3-Clause)
#   * Adobe-Japan1-UCS2 CID → Unicode mapping (Adobe mapping-resources-pdf,
#     BSD-3-Clause)
#   * Noto Sans JP Regular, the JP subset OTF (SIL Open Font License 1.1)
# into assets/cjk/ (git-ignored). Pinned to specific upstream commits so a
# build is reproducible; re-run after bumping the pins to update.
set -euo pipefail
cd "$(dirname "$0")/.."
dst=assets/cjk
mkdir -p "$dst/cmap"

CMAP_REV=${CMAP_REV:-refs/heads/master}
MAP_REV=${MAP_REV:-refs/heads/master}
NOTO_REV=${NOTO_REV:-refs/heads/main}
cmap_base="https://raw.githubusercontent.com/adobe-type-tools/cmap-resources/$CMAP_REV"
map_base="https://raw.githubusercontent.com/adobe-type-tools/mapping-resources-pdf/$MAP_REV"
noto_base="https://raw.githubusercontent.com/notofonts/noto-cjk/$NOTO_REV"

fetch() { # url dest
  if [ ! -s "$2" ]; then
    echo "fetch $1"
    curl -fsSL --retry 3 -o "$2.tmp" "$1" && mv "$2.tmp" "$2"
  fi
}

for n in H V 90ms-RKSJ-H 90ms-RKSJ-V 90msp-RKSJ-H 90msp-RKSJ-V 90pv-RKSJ-H 90pv-RKSJ-V \
         83pv-RKSJ-H Add-RKSJ-H Add-RKSJ-V Ext-RKSJ-H Ext-RKSJ-V EUC-H EUC-V \
         UniJIS-UCS2-H UniJIS-UCS2-V UniJIS-UCS2-HW-H UniJIS-UCS2-HW-V \
         UniJIS-UTF16-H UniJIS-UTF16-V UniJIS2004-UTF16-H UniJIS2004-UTF16-V \
         UniJISX0213-UTF32-H UniJISX0213-UTF32-V UniJISX02132004-UTF32-H UniJISX02132004-UTF32-V; do
  fetch "$cmap_base/Adobe-Japan1-7/CMap/$n" "$dst/cmap/$n"
done
fetch_any() { # dest url...
  local dest=$1; shift
  for u in "$@"; do
    if fetch "$u" "$dest" 2>/dev/null; then return 0; fi
  done
  echo "warning: none of the licence URLs for $dest could be fetched" >&2
}
fetch_any "$dst/cmap/LICENSE" "$cmap_base/LICENSE" "$cmap_base/LICENSE.md" "$cmap_base/Adobe-Japan1-7/LICENSE"
fetch "$map_base/pdf2unicode/Adobe-Japan1-UCS2" "$dst/cmap/Adobe-Japan1-UCS2"
fetch "$noto_base/Sans/SubsetOTF/JP/NotoSansJP-Regular.otf" "$dst/NotoSansJP-Regular.otf"
fetch_any "$dst/NotoSansJP-LICENSE" "$noto_base/Sans/LICENSE" "$noto_base/LICENSE" "$noto_base/Sans/OFL.txt"

echo "assets:"
du -sh "$dst"/* "$dst/cmap" | sed 's/^/  /'

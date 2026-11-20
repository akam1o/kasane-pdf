#!/bin/sh
# Courier-only Poppler reference (E1) for the Courier substitute
# evaluation (docs/courier-reference.md). Nothing is installed or
# changed system-wide.
#
#   scripts/courier-reference/setup-e1.sh DIR
#
# DIR/fonts   Nimbus Mono PS, the four styles, from the pinned
#             fonts-urw-base35 package (downloaded, checked, unpacked
#             here only — never installed)
# DIR/fonts.conf  a private cache, that directory and a strong
#             preference for Nimbus Mono PS for the family names Poppler
#             asks for Courier and Courier New — tested on the pattern as
#             Poppler builds it, before the system rules — then the
#             system configuration
# DIR/profile.json  the profile's name and version (the environment
#             fingerprint, scripts/compat_env.py, reads it)
# DIR/bin     pdftoppm, pdftotext, pdftocairo, pdfinfo wrappers running
#             the system tools with FONTCONFIG_FILE=DIR/fonts.conf — put
#             DIR/bin first on PATH for a reference process only
#
# Everything else resolves as without it (E0): scripts/courier-reference/
# probe.py checks that, and that the four Courier styles do resolve to
# these files, before any comparison.
set -eu
DIR=$(mkdir -p "$1" && cd "$1" && pwd)
VERSION=20200910-8build1
DEB_SHA256=be59254289c670d5c6580ce381995292aa05c9dd4bc5fa65b9dbaa527b55adda
mkdir -p "$DIR/deb" "$DIR/fonts" "$DIR/cache" "$DIR/bin"
cd "$DIR/deb"
if [ ! -f "fonts-urw-base35_${VERSION}_all.deb" ]; then
  apt-get download "fonts-urw-base35=$VERSION" > /dev/null
fi
echo "$DEB_SHA256  fonts-urw-base35_${VERSION}_all.deb" | sha256sum -c --quiet
rm -rf x
dpkg-deb -x "fonts-urw-base35_${VERSION}_all.deb" x
for s in Regular Bold Italic BoldItalic; do
  cp "x/usr/share/fonts/opentype/urw-base35/NimbusMonoPS-$s.otf" "$DIR/fonts/"
done
( cd "$DIR/fonts" && sha256sum -c --quiet ) <<EOF
4f225ca8e13acb16f733ce741693105e527d5f7a5443901b9ecc190fca4e149b  NimbusMonoPS-Regular.otf
f036d05d2168c7f71cb11d31e81d11133f3d09711e24ebde19d08a24842384d5  NimbusMonoPS-Bold.otf
7f1f85498027e07befadd3c7592518909b9e8c7f96ee5615e0b21be0c399c4b4  NimbusMonoPS-Italic.otf
a67ed9e364c933c79fc3ce88e17a0265334697c184b065407e754d43cbbf6d0a  NimbusMonoPS-BoldItalic.otf
EOF
rm -rf x
# Only these family names, compared as fontconfig compares them (case
# and spaces ignored): what Poppler asks for a non-embedded Courier,
# Courier New and their PostScript names.
rules=""
for fam in Courier "Courier New" "Courier New PS" "Courier New PSMT" CourierNewPSMT CourierNewPS; do
  rules="$rules
  <match target=\"pattern\">
    <test name=\"family\" qual=\"first\" compare=\"eq\"><string>$fam</string></test>
    <edit name=\"family\" mode=\"prepend\" binding=\"strong\"><string>Nimbus Mono PS</string></edit>
  </match>"
done
cat > "$DIR/fonts.conf" <<EOF
<?xml version="1.0"?>
<!DOCTYPE fontconfig SYSTEM "urn:fontconfig:fonts.dtd">
<fontconfig>
  <cachedir>$DIR/cache</cachedir>
  <dir>$DIR/fonts</dir>$rules
  <include ignore_missing="no">/etc/fonts/fonts.conf</include>
</fontconfig>
EOF
# What this directory is, for scripts/compat_env.py's fingerprint: raise
# the version with any change to the rules or the fonts.
printf '{"profile": "courier-nimbus-mono-ps", "version": 1}\n' > "$DIR/profile.json"
for t in pdftoppm pdftotext pdftocairo pdfinfo; do
  real=$(command -v "$t" || true)
  [ -n "$real" ] || { echo "setup-e1: no $t" >&2; exit 1; }
  printf '#!/bin/sh\nFONTCONFIG_FILE=%s exec %s "$@"\n' "$DIR/fonts.conf" "$real" > "$DIR/bin/$t"
  chmod +x "$DIR/bin/$t"
done
FONTCONFIG_FILE="$DIR/fonts.conf" fc-cache "$DIR/fonts" > /dev/null
echo "E1 ready in $DIR (fonts.conf sha256 $(sha256sum "$DIR/fonts.conf" | cut -c1-16)…)"

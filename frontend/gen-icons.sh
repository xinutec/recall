#!/usr/bin/env nix-shell
#!nix-shell -i bash -p librsvg imagemagick
# Regenerate the home app's raster icons from the SVG sources in public/.
# Source of truth: public/icon.svg (+ public/icon-maskable.svg). Edit those,
# then run ./gen-icons.sh from the frontend/ directory. The PNG/ICO outputs are
# git-tracked (the Angular build copies public/** verbatim), so commit them too.
set -euo pipefail
cd "$(dirname "$0")/public"

render() { rsvg-convert -w "$2" -h "$2" "$1" -o "$3"; }

# Browser tab favicon: multi-size .ico from the rounded source.
render icon.svg 16  /tmp/fav-16.png
render icon.svg 32  /tmp/fav-32.png
render icon.svg 48  /tmp/fav-48.png
magick /tmp/fav-16.png /tmp/fav-32.png /tmp/fav-48.png favicon.ico
rm -f /tmp/fav-16.png /tmp/fav-32.png /tmp/fav-48.png

# iOS home-screen + PWA manifest icons.
render icon.svg          180 apple-touch-icon.png
render icon.svg          192 icon-192.png
render icon.svg          512 icon-512.png
render icon-maskable.svg 512 icon-512-maskable.png

echo "generated: favicon.ico apple-touch-icon.png icon-192.png icon-512.png icon-512-maskable.png"

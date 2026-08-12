#!/usr/bin/env nix-shell
#!nix-shell -i bash -p librsvg imagemagick python3 python3Packages.pillow
# Regenerate recall's raster icons from the SVG sources in public/.
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

# A maskable icon is cropped by the launcher, not by us: everything outside the
# centred circle of 80% diameter can be cut off, and which shape is used is the
# launcher's choice. Eyeballing the square PNG cannot show that, and it is not a
# hypothetical — home's icon shipped for months reaching r=210 against a 204.8
# limit, with a round mask clipping the base of its house, and life's first cut
# had the same fault. The stroke's outer edge, not the path, is what the mask
# bites into. recall's mic currently sits at r=167, comfortably inside; this row
# is what keeps it there.
python3 - <<'PY'
import math, sys
from PIL import Image

SAFE = 512 * 0.8 / 2  # 204.8
im = Image.open("icon-512-maskable.png").convert("RGB")
c = (im.size[0] - 1) / 2
bg = im.getpixel((2, 2))  # full-bleed field colour
worst, at = 0.0, None
for y in range(im.size[1]):
    for x in range(im.size[0]):
        if sum(abs(a - b) for a, b in zip(im.getpixel((x, y)), bg)) > 90:
            d = math.hypot(x - c, y - c)
            if d > worst:
                worst, at = d, (x, y)
if worst > SAFE:
    sys.exit(
        f"icon-512-maskable.png: artwork reaches r={worst:.1f} at {at}, past the "
        f"{SAFE:.1f} safe zone — a round launcher mask would clip it. "
        f"Shrink the artwork in public/icon-maskable.svg."
    )
print(f"maskable safe zone ok: artwork reaches r={worst:.1f} of {SAFE:.1f}")
PY

echo "generated: favicon.ico apple-touch-icon.png icon-192.png icon-512.png icon-512-maskable.png"

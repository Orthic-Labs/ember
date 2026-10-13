#!/usr/bin/env python3
"""Regenerate background.tiff and dmg-layout.DS_Store for the macOS DMG window (see README.md here).

Asset-only tooling:
    pip install pillow==12.1.1 ds_store==1.3.3 mac_alias==2.2.3

Runs on any OS, no Mac needed. Both outputs come from this script alone, so they carry nothing
from the machine that made them: no local paths, user or disk names, dates or volume UUIDs.

    python3 packaging/macos/dmg/generate.py
"""

import datetime
import os
import sys
import tempfile

from ds_store import DSStore
from mac_alias import Alias, TargetInfo, VolumeInfo
from PIL import Image, ImageCms, ImageDraw, ImageFont
from pathlib import Path
import xml.etree.ElementTree as ET

HERE = os.path.dirname(os.path.abspath(__file__))

# Keep in sync with package.sh (volume name) and README.md (layout).
VOLUME = "Ember"
WIDTH, HEIGHT = 660, 400  # window content, pt; the background's 1x size
TITLE_BAR = 32
ICON_SIZE = 128
ICONS = {"Ember.app": (326, 205), "Applications": (574, 205)}
# A fixed date for the alias and the colour profile. Finder never matches it against the image
# (each build is a new volume): it finds the background by volume name and path.
FIXED_DATE = datetime.datetime(2026, 10, 8, tzinfo=datetime.timezone.utc)


def render(svg, width, out):
    # Render our small SVG vocabulary using only repository Inter & original Ember artwork.
    root = Path(HERE).parents[2]
    scale = width * 4 / WIDTH
    image = Image.new("RGB", (width * 4, int(HEIGHT * scale)))
    draw = ImageDraw.Draw(image)
    def coordinate(node, name, default=0):
        return float(node.attrib.get(name, default)) * scale
    for shape in ET.parse(svg).getroot():
        tag = shape.tag.rsplit("}", 1)[-1]
        if tag == "rect":
            x, y = coordinate(shape, "x"), coordinate(shape, "y")
            draw.rectangle((x, y, x + coordinate(shape, "width") - 1, y + coordinate(shape, "height") - 1), fill=shape.attrib["fill"])
        elif tag == "image":
            source = (Path(svg).parent / shape.attrib["href"]).resolve()
            if source != (root / "assets/app-icon/ember-reverse-1024.png").resolve():
                raise ValueError("only original Ember artwork is supported")
            with Image.open(source) as original:
                mark = original.convert("RGBA").resize((round(coordinate(shape, "width")), round(coordinate(shape, "height"))), Image.Resampling.LANCZOS)
            image.paste(mark, (round(coordinate(shape, "x")), round(coordinate(shape, "y"))), mark)
        elif tag == "line":
            draw.line((coordinate(shape, "x1"), coordinate(shape, "y1"), coordinate(shape, "x2"), coordinate(shape, "y2")), fill=shape.attrib["stroke"], width=round(coordinate(shape, "stroke-width", 1)))
        elif tag == "text":
            face = "Inter-SemiBold.ttf" if shape.attrib.get("font-weight") == "600" else "Inter-Regular.ttf"
            font = ImageFont.truetype(str(root / "assets/fonts" / face), round(coordinate(shape, "font-size")))
            draw.text((coordinate(shape, "x"), coordinate(shape, "y")), shape.text or "", font=font, anchor="ls", fill=shape.attrib["fill"])
        else:
            raise ValueError(f"unsupported background shape: {tag}")
    image = image.resize((width, round(HEIGHT * width / WIDTH)), Image.Resampling.LANCZOS)
    image.save(out)
    return image.copy()  # Drop PNG encoder settings before TIFF serialization.


def srgb_profile():
    # LittleCMS's built-in sRGB ("No copyright, use freely"), with its creation date fixed.
    icc = bytearray(ImageCms.ImageCmsProfile(ImageCms.createProfile("sRGB")).tobytes())
    d = FIXED_DATE
    icc[24:36] = b"".join(v.to_bytes(2, "big") for v in (d.year, d.month, d.day, 0, 0, 0))
    if icc[84:100] != bytes(16):
        sys.exit("error: the sRGB profile has an ID; recompute it after fixing the date")
    return bytes(icc)


def write_tiff(svg, out):
    """1x (72 dpi) and 2x (144 dpi) in one TIFF, as `tiffutil -cathidpicheck` writes them."""
    with tempfile.TemporaryDirectory() as tmp:
        one, two = (render(svg, WIDTH * s, os.path.join(tmp, f"{s}x.png")) for s in (1, 2))
    two.encoderinfo = {"dpi": (144, 144)}  # the rest as the first page
    # Opaque RGB, deterministic Deflate compression.
    one.save(
        out,
        "TIFF",
        save_all=True,
        append_images=[two],
        compression="tiff_adobe_deflate",  # TIFF compression 8 (Deflate)
        dpi=(72, 72),
        icc_profile=srgb_profile(),
    )
    with Image.open(out) as im:
        im.seek(1)
        if im.info.get("dpi") != (144, 144):
            sys.exit("error: the 2x page isn't 144 dpi (needs Pillow 12 or later)")
        strips = []
        for page in (0, 1):
            im.seek(page)
            strips += zip(im.tag_v2[273], im.tag_v2[279])  # StripOffsets, StripByteCounts
    # TIFF keeps directories and tag data on even offsets. When a strip ends on an odd one, libtiff
    # leaves the padding byte after it uninitialised, so the file changed from run to run with
    # identical pixels. Zero it: nothing refers to that byte.
    starts = {o for o, _ in strips}
    with open(out, "r+b") as f:
        for o, n in strips:
            if (o + n) % 2 and o + n not in starts:
                f.seek(o + n)
                f.write(bytes(1))


def background_alias():
    # What Finder stores for /.background/background.tiff on the volume, minus anything about the
    # machine (dmgbuild writes the same kind of alias from a mounted image). Disk type 5 (ejectable),
    # the volume flags and the folder/file ids (25, 26) are what Finder wrote on a test image; the ids
    # are only hints, as the volume's creation date.
    volume = VolumeInfo(VOLUME, FIXED_DATE, b"H+", 5, 0x0D02, b"\0\0")
    volume.posix_path = f"/Volumes/{VOLUME}"
    target = TargetInfo(0, "background.tiff", 25, 26, FIXED_DATE, b"\0\0\0\0", b"\0\0\0\0")
    target.folder_name = ".background"
    target.cnid_path = [25]
    target.carbon_path = f"{VOLUME}:.background:\0background.tiff"
    target.posix_path = "/.background/background.tiff"
    return Alias(volume=volume, target=target).to_bytes()


def write_ds_store(out):
    if os.path.exists(out):
        os.remove(out)
    with DSStore.open(out, "w+") as d:
        d["."]["bwsp"] = {
            "ShowStatusBar": False,
            "ShowToolbar": False,
            "ShowTabView": False,
            "ContainerShowSidebar": False,
            "ShowSidebar": False,
            "WindowBounds": f"{{{{200, 528}}, {{{WIDTH}, {HEIGHT + TITLE_BAR}}}}}",
        }
        d["."]["icvp"] = {
            "viewOptionsVersion": 1,
            "backgroundType": 2,
            "backgroundImageAlias": background_alias(),
            "backgroundColorRed": 1.0,
            "backgroundColorGreen": 1.0,
            "backgroundColorBlue": 1.0,
            "gridOffsetX": 0.0,
            "gridOffsetY": 0.0,
            "gridSpacing": 100.0,
            "arrangeBy": "none",
            "showIconPreview": True,
            "showItemInfo": False,
            "labelOnBottom": True,
            "textSize": 16.0,
            "iconSize": float(ICON_SIZE),
        }
        d["."]["vSrn"] = ("long", 1)
        for name, pos in ICONS.items():
            d[name]["Iloc"] = pos


def main():
    write_tiff(os.path.join(HERE, "background.svg"), os.path.join(HERE, "background.tiff"))
    write_ds_store(os.path.join(HERE, "dmg-layout.DS_Store"))


if __name__ == "__main__":
    main()

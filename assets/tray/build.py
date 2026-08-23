#!/usr/bin/env python3
"""Generate the Parle tray/menu-bar icon set.

Draws one parametric glyph (microphone inside a rounded-square outline, echoing
the app badge) as SVG, writes a source SVG per variant into assets/tray/, then
rasterises each to 44x44 and 88x88 PNGs in src-tauri/icons/ using headless Edge
(vector -> exact-size raster, no resampling), and finally builds a contact sheet
at assets/tray/preview.png showing every icon at 16/20/24/44 on light and dark.

Requires: Pillow, Microsoft Edge.
    python assets/tray/build.py
"""

import os
import subprocess
import sys

from PIL import Image

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
ICONS = os.path.join(REPO, "src-tauri", "icons")
EDGE = r"C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe"

# ---------------------------------------------------------------- geometry --
# 24x24 design grid, Lucide-ish weights (~1.8-1.9 stroke at 24).
# The binding constraint is the 16px render: every gap between two inked shapes
# must stay >= ~1.2 design units (0.8px at 16) or the antialiasing fuses them.
FRAME_INSET = 2.4      # rounded-square outline; inner clear area = 3.25 .. 20.75
FRAME_RX = 5.8
FRAME_W = 1.7

BODY_W = 4.6           # mic capsule (filled: an outlined capsule closes up <20px)
BODY_H = 7.4           # 1.6:1 — squatter than this and it stops reading as a mic
BODY_Y = 5.1           # -> 5.1 .. 12.5

ARC_R = 5.0            # cradle: half-circle under the capsule, bottom at 15.2
ARC_Y = 10.2           # endpoints sit at the capsule's lower third
ARC_W = 1.7            # 1.85u clear between arc sides and capsule; 2.9u to frame

STEM_W = 1.7
STEM_Y = 15.6          # overlaps the arc bottom so they weld
STEM_H = 2.8

DOT_CX = 19.6          # recording badge, top-right corner of the frame
DOT_CY = 4.4
DOT_R = 2.2
DOT_GAP = 0.8          # transparent knockout ring around the dot


def mic(fg):
    """Capsule + cradle + stem, no frame. Shared by the outline and badge marks."""
    return (
        f'<rect x="{12 - BODY_W / 2}" y="{BODY_Y}" width="{BODY_W}" height="{BODY_H}" '
        f'rx="{BODY_W / 2}" fill="{fg}"/>'
        f'<path d="M{12 - ARC_R} {ARC_Y} A{ARC_R} {ARC_R} 0 0 0 {12 + ARC_R} {ARC_Y}" '
        f'fill="none" stroke="{fg}" stroke-width="{ARC_W}" stroke-linecap="round"/>'
        f'<rect x="{12 - STEM_W / 2}" y="{STEM_Y}" width="{STEM_W}" height="{STEM_H}" '
        f'rx="{STEM_W / 2}" fill="{fg}"/>'
    )


def glyph(fg):
    fs = 24 - 2 * FRAME_INSET
    return (
        f'<rect x="{FRAME_INSET}" y="{FRAME_INSET}" width="{fs}" height="{fs}" '
        f'rx="{FRAME_RX}" fill="none" stroke="{fg}" stroke-width="{FRAME_W}"/>'
        + mic(fg)
    )


# Filled-badge mark: a miniature of the app icon. Solid blue rounded square with
# a white mic on top and no outline, so it reads the same on light and dark
# taskbars without needing a per-theme pair.
BADGE_INSET = 1.3          # fills more of the tile than the outline mark
BADGE_RX = 6.6
BADGE_TOP = "#3d6bff"      # the app badge's own gradient stops
BADGE_MID = "#2b5cff"
BADGE_BOT = "#1a3fd6"


def badge_svg(dot=None):
    bs = 24 - 2 * BADGE_INSET
    grad = (
        '<linearGradient id="bg" x1="0" y1="0" x2="1" y2="1">'
        f'<stop offset="0%" stop-color="{BADGE_TOP}"/>'
        f'<stop offset="55%" stop-color="{BADGE_MID}"/>'
        f'<stop offset="100%" stop-color="{BADGE_BOT}"/>'
        "</linearGradient>"
    )
    body = (
        f'<rect x="{BADGE_INSET}" y="{BADGE_INSET}" width="{bs}" height="{bs}" '
        f'rx="{BADGE_RX}" fill="url(#bg)"/>' + mic("#ffffff")
    )
    if dot is None:
        return (
            '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" '
            f'width="24" height="24"><defs>{grad}</defs>{body}</svg>'
        )
    defs = (
        f"<defs>{grad}</defs>"
        '<mask id="badgecut">'
        '<rect x="0" y="0" width="24" height="24" fill="#fff"/>'
        f'<circle cx="{DOT_CX}" cy="{DOT_CY}" r="{DOT_R + DOT_GAP}" fill="#000"/>'
        "</mask>"
    )
    return (
        '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" '
        f'width="24" height="24">{defs}'
        f'<g mask="url(#badgecut)">{body}</g>'
        f'<circle cx="{DOT_CX}" cy="{DOT_CY}" r="{DOT_R}" fill="{dot}"/></svg>'
    )


def svg(fg, dot=None):
    """dot=None -> idle. dot='#rrggbb' -> recording badge (knocked out of the glyph)."""
    g = glyph(fg)
    if dot is None:
        inner = g
        defs = ""
    else:
        defs = (
            '<mask id="badge">'
            '<rect x="0" y="0" width="24" height="24" fill="#fff"/>'
            f'<circle cx="{DOT_CX}" cy="{DOT_CY}" r="{DOT_R + DOT_GAP}" fill="#000"/>'
            "</mask>"
        )
        inner = (
            f'<g mask="url(#badge)">{g}</g>'
            f'<circle cx="{DOT_CX}" cy="{DOT_CY}" r="{DOT_R}" fill="{dot}"/>'
        )
    return (
        '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" '
        f'width="24" height="24">{defs}{inner}</svg>'
    )


# ---------------------------------------------------------------- variants --
TEMPLATE_FG = "#000000"   # macOS template: alpha-only, colour is discarded
LIGHT_FG = "#ffffff"      # for dark Windows taskbars
DARK_FG = "#1c1c1e"       # for light Windows taskbars
COLOR_FG = "#3d6bff"      # app-badge blue (top gradient stop; brighter = survives dark taskbars)
REC_RED = "#ff3b30"

VARIANTS = [
    # (basename,                fg,           dot)
    ("tray",                    TEMPLATE_FG,  None),
    # Template images are alpha-only, so the recording dot must be black too;
    # it reads because of the transparent knockout ring around it.
    ("tray-recording",          TEMPLATE_FG,  TEMPLATE_FG),
    ("tray-light",              LIGHT_FG,     None),
    ("tray-light-recording",    LIGHT_FG,     REC_RED),
    ("tray-dark",               DARK_FG,      None),
    ("tray-dark-recording",     DARK_FG,      REC_RED),
    ("tray-color",              COLOR_FG,     None),
    ("tray-color-recording",    COLOR_FG,     REC_RED),
]

# Filled badge — built by badge_svg() rather than svg(), so it lives apart.
BADGE_VARIANTS = [
    ("tray-badge",              None),
    ("tray-badge-recording",    REC_RED),
]

SIZES = {"": 44, "@2x": 88}

# Every mark that gets rasterised, in strip order.
ALL_NAMES = [v[0] for v in VARIANTS] + [v[0] for v in BADGE_VARIANTS]


def edge(html_path, w, h, out, scale=1):
    subprocess.run(
        [
            EDGE, "--headless=new", "--disable-gpu", "--no-sandbox", "--hide-scrollbars",
            "--default-background-color=00000000",
            "--force-device-scale-factor=%d" % scale,
            "--window-size=%d,%d" % (w, h),
            "--screenshot=" + out,
            "file:///" + html_path.replace("\\", "/"),
        ],
        capture_output=True,
    )
    if not os.path.exists(out):
        sys.exit("render failed: " + out)


def main():
    os.makedirs(ICONS, exist_ok=True)

    # 1. SVG sources.
    for name, fg, dot in VARIANTS:
        open(os.path.join(HERE, name + ".svg"), "w").write(svg(fg, dot))
    for name, dot in BADGE_VARIANTS:
        open(os.path.join(HERE, name + ".svg"), "w").write(badge_svg(dot))

    # 2. Rasterise. One strip per size, then crop each cell out — exact pixel
    #    sizes straight from the vector, no resampling.
    for suffix, size in SIZES.items():
        imgs = "".join(
            '<img src="%s.svg" width="%d" height="%d">' % (n, size, size)
            for n in ALL_NAMES
        )
        html = (
            "<html><head><style>html,body{margin:0;padding:0;background:transparent}"
            "body{white-space:nowrap;font-size:0}img{display:inline-block;vertical-align:top}"
            "</style></head><body>" + imgs + "</body></html>"
        )
        hp = os.path.join(HERE, "_strip%d.html" % size)
        open(hp, "w").write(html)
        strip = os.path.join(HERE, "_strip%d.png" % size)
        edge(hp, len(ALL_NAMES) * size + 60, size + 40, strip)
        sheet = Image.open(strip).convert("RGBA")
        for i, name in enumerate(ALL_NAMES):
            cell = sheet.crop((i * size, 0, (i + 1) * size, size))
            cell.save(os.path.join(ICONS, "%s%s.png" % (name, suffix)))
        os.remove(hp)
        os.remove(strip)

    # 3. Contact sheet from the SHIPPED 44px PNGs (i.e. what the OS actually
    #    scales down), at every real tray size, on light and dark.
    prev_sizes = [16, 20, 24, 44]
    rows = ""
    for name in ALL_NAMES:
        src = "file:///" + os.path.join(ICONS, name + ".png").replace("\\", "/")
        cells = "".join(
            '<div class="c"><img src="%s" width="%d" height="%d"><em>%d</em></div>' % (src, s, s, s)
            for s in prev_sizes
        )
        rows += (
            '<div class="row"><span class="lbl">%s</span>'
            '<div class="strip light">%s</div>'
            '<div class="strip dark">%s</div></div>' % (name, cells, cells)
        )

    html = """<html><head><style>
html,body{margin:0;padding:0;background:#6f7378;font:12px -apple-system,Segoe UI,system-ui}
h1{color:#fff;font-size:13px;margin:14px 0 8px 16px;letter-spacing:.04em;text-transform:uppercase}
.row{display:flex;align-items:center;gap:10px;padding:4px 16px}
.lbl{width:168px;color:#fff;font-weight:600;font-size:11px}
.strip{display:flex;align-items:flex-end;gap:16px;padding:8px 16px;border-radius:8px}
.light{background:#f4f4f5}.dark{background:#1b1c1e}
.c{display:flex;flex-direction:column;align-items:center;gap:3px}
.c em{font-style:normal;font-size:8px;opacity:.45}
.light .c em{color:#000}.dark .c em{color:#fff}
</style></head><body><h1>Parle tray icons &mdash; shipped 44px PNGs at real tray sizes</h1>""" + rows + "</body></html>"

    hp = os.path.join(HERE, "_preview.html")
    open(hp, "w").write(html)
    edge(hp, 700, 720, os.path.join(HERE, "preview.png"), scale=2)
    os.remove(hp)
    print("wrote %d PNGs to %s" % (len(VARIANTS) * len(SIZES), ICONS))
    print("preview: " + os.path.join(HERE, "preview.png"))


if __name__ == "__main__":
    main()

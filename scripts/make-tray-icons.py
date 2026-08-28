#!/usr/bin/env python3
"""Generate Parle's tray/menu-bar icons.

The icons are BUILT, not hand-drawn, so the whole set stays consistent and any
tweak is one edit here rather than twenty PNGs re-exported by hand.

Shape: a filled rounded square with the microphone knocked OUT of it, so the
mic reads as transparent. That is what makes it work as a macOS template image:
macOS recolours the opaque pixels to suit the menu bar, and the holes stay
holes, in light mode and dark mode alike.

Recording adds a knocked-out dot in the top-right corner. It has to be a hole
rather than a coloured dot for the same reason: in a template image every
opaque pixel is painted the same colour, so a black dot on a black square would
be invisible.

    python3 scripts/make-tray-icons.py
"""
from PIL import Image, ImageDraw

SS = 8  # supersample factor, downscaled at the end for smooth edges


def draw_icon(size: int, recording: bool, rgb=(0, 0, 0)) -> Image.Image:
    S = size * SS
    img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)

    # The plate.
    inset = S * 0.03
    radius = S * 0.26
    d.rounded_rectangle([inset, inset, S - inset, S - inset], radius=radius,
                        fill=rgb + (255,))

    # The microphone, punched out. Drawn into a mask and then subtracted, so
    # the strokes join cleanly instead of over-painting each other's alpha.
    mask = Image.new("L", (S, S), 0)
    m = ImageDraw.Draw(mask)

    cx = S / 2
    stroke = S * 0.075

    # Capsule body.
    body_w = S * 0.21
    body_top = S * 0.21
    body_bot = S * 0.50
    m.rounded_rectangle([cx - body_w / 2, body_top, cx + body_w / 2, body_bot],
                        radius=body_w / 2, fill=255)

    # Cradle: a shallow U hugging the body, drawn as the lower half of an
    # ellipse so its ends rise to either side of the capsule.
    cradle_w = S * 0.44
    cradle_h = S * 0.30
    cradle_cy = S * 0.47
    m.arc([cx - cradle_w / 2, cradle_cy - cradle_h / 2,
           cx + cradle_w / 2, cradle_cy + cradle_h / 2],
          start=0, end=180, fill=255, width=int(stroke))

    # Stem, from the bottom of the cradle to the base.
    stem_top = cradle_cy + cradle_h / 2 - stroke * 0.5
    stem_bot = S * 0.78
    m.rectangle([cx - stroke / 2, stem_top, cx + stroke / 2, stem_bot], fill=255)

    # Base bar.
    base_w = S * 0.30
    m.rounded_rectangle([cx - base_w / 2, stem_bot - stroke / 2,
                         cx + base_w / 2, stem_bot + stroke / 2],
                        radius=stroke / 2, fill=255)

    if recording:
        # The recording badge, top-right, punched out like the mic.
        r = S * 0.115
        pad = S * 0.115
        m.ellipse([S - pad - r * 2, pad, S - pad, pad + r * 2], fill=255)

    # Subtract the mask from the plate's alpha.
    alpha = img.getchannel("A")
    holes = mask.point(lambda v: 255 - v)
    img.putalpha(Image.composite(alpha, Image.new("L", (S, S), 0), holes))

    return img.resize((size, size), Image.LANCZOS)


def main() -> None:
    out = "src-tauri/icons"
    variants = [
        ("tray", (0, 0, 0)),          # macOS template (colour is ignored by the OS)
        ("tray-dark", (0, 0, 0)),     # for light taskbars
        ("tray-light", (255, 255, 255)),  # for dark taskbars
    ]
    for name, rgb in variants:
        for rec in (False, True):
            stem = f"{name}-recording" if rec else name
            for size, suffix in ((44, ""), (88, "@2x")):
                path = f"{out}/{stem}{suffix}.png"
                draw_icon(size, rec, rgb).save(path)
                print("wrote", path)


if __name__ == "__main__":
    main()

"""Generates Tallypad's icon set (ICO for Windows, PNGs for the site/README).

Usage:  python assets/make_icon.py
Requires Pillow.
"""

from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter

HERE = Path(__file__).parent
S = 1024  # master canvas; downsampled for anti-aliasing


def lerp(a, b, t):
    return tuple(int(a[i] + (b[i] - a[i]) * t) for i in range(len(a)))


def vertical_gradient(size, top, bottom):
    img = Image.new("RGBA", (size, size))
    px = img.load()
    for y in range(size):
        c = lerp(top, bottom, y / (size - 1))
        for x in range(size):
            px[x, y] = c
    return img


def rounded_mask(size, box, radius):
    m = Image.new("L", (size, size), 0)
    ImageDraw.Draw(m).rounded_rectangle(box, radius=radius, fill=255)
    return m


def make_master():
    canvas = Image.new("RGBA", (S, S), (0, 0, 0, 0))

    # Soft drop shadow
    pad = 64
    tile_box = (pad, pad, S - pad, S - pad)
    radius = 210
    shadow = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    ImageDraw.Draw(shadow).rounded_rectangle(
        (pad, pad + 28, S - pad, S - pad + 28), radius=radius, fill=(0, 0, 0, 150)
    )
    canvas.alpha_composite(shadow.filter(ImageFilter.GaussianBlur(28)))

    # Tile: deep slate gradient
    tile = vertical_gradient(S, (44, 48, 60, 255), (20, 21, 26, 255))
    canvas.paste(tile, (0, 0), rounded_mask(S, tile_box, radius))

    # Translucent strokes go on their own layer so they blend instead of
    # punching holes in the alpha channel.
    glow = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    g = ImageDraw.Draw(glow)
    g.rounded_rectangle(tile_box, radius=radius, outline=(255, 255, 255, 30), width=6)

    # Results column
    split = 610
    col = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    ImageDraw.Draw(col).rectangle((split, pad, S - pad, S - pad), fill=(255, 255, 255, 10))
    canvas.alpha_composite(Image.composite(col, Image.new("RGBA", (S, S)), rounded_mask(S, tile_box, radius)))
    g.rectangle((split - 3, pad + 120, split + 3, S - pad - 120), fill=(255, 255, 255, 40))
    canvas.alpha_composite(glow)
    d = ImageDraw.Draw(canvas)

    # Text lines on the left, results on the right
    rows = [(300, 330), (440, 240), (580, 300)]
    for y, length in rows:
        d.rounded_rectangle((170, y - 26, 170 + length, y + 26), radius=26, fill=(214, 218, 228, 235))
    for i, (y, _) in enumerate(rows):
        w = [150, 110, 170][i]
        d.rounded_rectangle((S - 170 - w, y - 26, S - 170, y + 26), radius=26, fill=(95, 212, 160, 255))

    # Big mint "=" with a total bar underneath
    eq_y = 740
    d.rounded_rectangle((170, eq_y - 52, 470, eq_y - 10), radius=21, fill=(95, 212, 160, 255))
    d.rounded_rectangle((170, eq_y + 22, 470, eq_y + 64), radius=21, fill=(95, 212, 160, 255))
    d.rounded_rectangle((S - 170 - 230, eq_y - 40, S - 170, eq_y + 52), radius=46, fill=(95, 212, 160, 255))
    return canvas


def main():
    master = make_master()
    sizes = [16, 20, 24, 32, 40, 48, 64, 128, 256]
    master.resize((256, 256), Image.LANCZOS).save(
        HERE / "tallypad.ico", sizes=[(s, s) for s in sizes]
    )
    master.resize((512, 512), Image.LANCZOS).save(HERE / "tallypad-512.png")
    master.resize((128, 128), Image.LANCZOS).save(HERE / "tallypad-128.png")
    master.resize((32, 32), Image.LANCZOS).save(HERE / "favicon-32.png")
    print("wrote icons to", HERE)


if __name__ == "__main__":
    main()

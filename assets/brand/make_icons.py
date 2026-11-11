"""Renders the raster icons from the geometry of static/brand/app-icon.svg.

Run from the repository root:  python assets/brand/make_icons.py
Needs Pillow. The shapes below mirror the SVG, on its 64-unit grid.
"""
from PIL import Image, ImageDraw

BG, TEAL, AMBER = (14, 21, 19, 255), (45, 212, 191, 255), (245, 158, 11, 255)
SS = 8  # supersampling factor


def bezier(p0, p1, p2, p3, n=48):
    pts = []
    for i in range(n + 1):
        t = i / n
        u = 1 - t
        pts.append(tuple(u**3 * a + 3 * u * u * t * b + 3 * u * t * t * c + t**3 * d for a, b, c, d in zip(p0, p1, p2, p3)))
    return pts


def stroke(draw, pts, width, color, k):
    r = width * k / 2
    for (x0, y0), (x1, y1) in zip(pts, pts[1:]):
        draw.line([(x0 * k, y0 * k), (x1 * k, y1 * k)], fill=color, width=round(width * k))
    for x, y in pts:
        draw.ellipse([x * k - r, y * k - r, x * k + r, y * k + r], fill=color)


def render(size, tile=True):
    big = size * SS
    k = big / 64
    img = Image.new("RGBA", (big, big), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    if tile:
        d.rounded_rectangle([0, 0, big - 1, big - 1], radius=14 * k, fill=BG)
    w = 5 if size >= 48 else 6.5
    r = 5 if size >= 48 else 6
    stroke(d, [(24, 17), (24, 47)], w, TEAL, k)
    branch = [(41, 19), (41, 25)] + bezier((41, 25), (41, 36), (24, 32), (24, 44))
    stroke(d, branch, w, AMBER, k)
    for (x, y), c in (((24, 17), TEAL), ((24, 47), TEAL), ((41, 19), AMBER)):
        d.ellipse([(x - r) * k, (y - r) * k, (x + r) * k, (y + r) * k], fill=c)
    return img.resize((size, size), Image.LANCZOS)


if __name__ == "__main__":
    for s in (128, 256, 512, 1024):
        render(s).save(f"assets/icons/graftline-{s}.png")
    render(256).save("assets/graftline.ico", sizes=[(s, s) for s in (16, 24, 32, 48, 64, 128, 256)])
    with open("assets/window-icon-64.rgba", "wb") as f:
        f.write(render(64).tobytes())
    render(192).save("docs/assets/icon-192.png")
    render(512).save("docs/assets/icon-512.png")
    render(180).save("docs/assets/apple-touch-icon.png")
    render(48).save("docs/assets/favicon.ico", sizes=[(16, 16), (32, 32), (48, 48)])
    print("icons written")

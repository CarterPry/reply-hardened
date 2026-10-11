from PIL import Image, ImageDraw, ImageFilter
import math

def bubble_mask(S, cx, cy, w, h, tail=True, ss=1):
    """Speech bubble: ellipse + lower-left tail, as an L mask of size S."""
    m = Image.new("L", (S, S), 0)
    d = ImageDraw.Draw(m)
    d.ellipse([cx - w/2, cy - h/2, cx + w/2, cy + h/2], fill=255)
    if tail:
        ang_bot, ang_side = math.radians(100), math.radians(158)
        p0 = (cx + (w/2)*math.cos(ang_bot), cy + (h/2)*math.sin(ang_bot))    # bottom attach
        p1 = (cx + (w/2)*math.cos(ang_side), cy + (h/2)*math.sin(ang_side))  # left-side attach
        tip = (cx - w*0.49, cy + h*0.61)
        # left edge p1 -> tip: convex, continues the ellipse's curvature outward
        c1 = (p1[0] + w*0.015, p1[1] + h*0.24)
        # bottom edge tip -> p0: concave scoop back into the bubble
        mid = ((tip[0]+p0[0])/2, (tip[1]+p0[1])/2)
        c0 = (mid[0] + w*0.10, mid[1] - h*0.16)
        pts = []
        n = 40
        for i in range(n+1):
            t = i/n
            pts.append(((1-t)**2*p1[0] + 2*(1-t)*t*c1[0] + t**2*tip[0],
                        (1-t)**2*p1[1] + 2*(1-t)*t*c1[1] + t**2*tip[1]))
        for i in range(n+1):
            t = i/n
            pts.append(((1-t)**2*tip[0] + 2*(1-t)*t*c0[0] + t**2*p0[0],
                        (1-t)**2*tip[1] + 2*(1-t)*t*c0[1] + t**2*p0[1]))
        pts.append((cx, cy))
        d.polygon(pts, fill=255)
    return m

def app_icon(size=1024, ss=4):
    S = size*ss
    # green vertical gradient, iMessage-style
    top, bot = (0x6A, 0xF7, 0x86), (0x0D, 0xB9, 0x2E)
    grad = Image.new("RGB", (1, S))
    for y in range(S):
        t = y/(S-1)
        grad.putpixel((0, y), tuple(int(top[i]*(1-t)+bot[i]*t) for i in range(3)))
    grad = grad.resize((S, S))
    # rounded-square mask (macOS keeps ~10% transparent margin around the tile)
    margin = int(S*0.10)
    r = int((S-2*margin)*0.225)
    sq = Image.new("L", (S, S), 0)
    ImageDraw.Draw(sq).rounded_rectangle([margin, margin, S-margin-1, S-margin-1], radius=r, fill=255)
    out = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    out.paste(grad, (0, 0), sq)
    # white bubble
    cx, cy = S/2, S*0.485
    bw, bh = S*0.60, S*0.52
    bm = bubble_mask(S, cx, cy, bw, bh)
    white = Image.new("RGBA", (S, S), (255, 255, 255, 255))
    out.paste(white, (0, 0), bm)
    return out.resize((size, size), Image.LANCZOS)

def tray_icon(size=72):
    """Template image: black bubble silhouette on transparent, 18pt box."""
    ss = 8
    S = size*ss
    cx, cy = S/2, S*0.47
    bw, bh = S*0.86, S*0.72
    bm = bubble_mask(S, cx, cy, bw, bh)
    out = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    out.paste(Image.new("RGBA", (S, S), (0, 0, 0, 255)), (0, 0), bm)
    return out.resize((size, size), Image.LANCZOS)

app_icon().save("out/icon-1024.png")
tray_icon(72).save("out/tray.png")
tray_icon(36).save("out/tray-36.png")
# preview sheet
sheet = Image.new("RGBA", (1024+200, 1024), (240, 240, 240, 255))
sheet.paste(app_icon(), (0, 0), app_icon())
t = tray_icon(144)
sheet.paste(t, (1050, 40), t)
sheet.save("out/preview.png")
print("ok")

# Usage (from the repo root):
#   mkdir -p out && python3 tools/make-icons.py
#   npx tauri icon out/icon-1024.png -o src-tauri/icons && rm -rf src-tauri/icons/{android,ios}
#   cp out/tray.png src-tauri/icons/tray.png

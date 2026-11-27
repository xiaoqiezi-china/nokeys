from PIL import Image, ImageDraw
import math, os

S = 1024
img = Image.new("RGBA", (S, S), (0, 0, 0, 0))

# brand gradient disc
grad = Image.new("RGBA", (S, S), (0, 0, 0, 0))
gd = ImageDraw.Draw(grad)
top = (0x86, 0xa8, 0xff)
mid = (0x5b, 0x8c, 0xff)
bot = (0x3f, 0x6f, 0xe0)
for y in range(S):
    t = y / (S - 1)
    # two-stop blend through the middle colour (like the settings brand square)
    if t < 0.55:
        k = t / 0.55
        c = tuple(int(top[i] + (mid[i] - top[i]) * k) for i in range(3))
    else:
        k = (t - 0.55) / 0.45
        c = tuple(int(mid[i] + (bot[i] - mid[i]) * k) for i in range(3))
    gd.line([(0, y), (S, y)], fill=c + (255,))

disc = Image.new("L", (S, S), 0)
ImageDraw.Draw(disc).ellipse([S * 0.03, S * 0.03, S * 0.97, S * 0.97], fill=255)
img.paste(grad, (0, 0), disc)

d = ImageDraw.Draw(img)
c = S / 2
# white ring
r_out, r_in = S * 0.285, S * 0.205
d.ellipse([c - r_out, c - r_out, c + r_out, c + r_out], fill=(255, 255, 255, 255))
d.ellipse([c - r_in, c - r_in, c + r_in, c + r_in], fill=(0, 0, 0, 0))
# re-punch the inner hole with the disc colour: transparent would show the desktop
hole = Image.new("RGBA", (S, S), (0, 0, 0, 0))
hole.paste(grad, (0, 0), disc)
mask = Image.new("L", (S, S), 0)
ImageDraw.Draw(mask).ellipse([c - r_in, c - r_in, c + r_in, c + r_in], fill=255)
img.paste(hole, (0, 0), mask)
# centre dot
r_dot = S * 0.085
d.ellipse([c - r_dot, c - r_dot, c + r_dot, c + r_dot], fill=(255, 255, 255, 255))

big = img.resize((S, S), Image.LANCZOS)
out = "D:/project/nokeys/ui"
assets = "D:/project/nokeys/assets"
os.makedirs(assets, exist_ok=True)
big.resize((256, 256), Image.LANCZOS).save(os.path.join(out, "app.png"))
big.save(os.path.join(os.path.dirname(out), "assets", "app.ico"), sizes=[(256, 256), (128, 128), (64, 64), (48, 48), (32, 32), (24, 24), (16, 16)])
print("sizes:", big.size, "ico bytes:", os.path.getsize(os.path.join(out, "app.ico")))

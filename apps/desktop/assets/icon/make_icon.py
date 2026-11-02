"""Exports gesso-icon.canvas with `canvas export` and packs the sizes into gesso.ico, with
gesso-256.png and gesso-32.png for the window. Sizes up to 32 use the simplified artwork;
larger ones are downsampled from a full-size export, which keeps the weave from banding.
Run from this folder, after a release build of the `canvas` CLI. Needs Pillow."""
import io, json, os, struct, subprocess, tempfile

from PIL import Image

CANVAS = os.path.join("..", "..", "..", "..", "target", "release", "canvas.exe")
SIZES = [16, 20, 24, 32, 40, 48, 64, 128, 256]


def frame_id(node, name):
    if isinstance(node, dict):
        if node.get("name") == name:
            return node["id"]
        node = list(node.values())
    if isinstance(node, list):
        for v in node:
            found = frame_id(v, name)
            if found:
                return found
    return None


doc = json.load(open(os.path.join("gesso-icon.canvas", "document.json"), encoding="utf-8"))


def export(tmp, name, size):
    out = os.path.join(tmp, f"{name}-{size}.png")
    subprocess.run([CANVAS, "export", "gesso-icon.canvas", "--node", frame_id(doc, name), "--format", "png", "--scale", str(size / 1024), "--padding", "0", "-o", out], check=True, capture_output=True)
    return out


images = []
with tempfile.TemporaryDirectory() as tmp:
    full = Image.open(export(tmp, "Gesso icon", 1024)).convert("RGBA")
    for s in SIZES:
        if s <= 32:
            images.append(open(export(tmp, "Gesso icon small", s), "rb").read())
        else:
            out = io.BytesIO()
            full.resize((s, s), Image.LANCZOS).save(out, "PNG", optimize=True)
            images.append(out.getvalue())

for s, png in zip(SIZES, images):
    w, h = struct.unpack(">II", png[16:24])
    assert (w, h) == (s, s), (s, w, h)

offset = 6 + 16 * len(images)
ico = struct.pack("<HHH", 0, 1, len(images))
for s, png in zip(SIZES, images):
    ico += struct.pack("<BBBBHHII", s % 256, s % 256, 0, 0, 1, 32, len(png), offset)
    offset += len(png)
open("gesso.ico", "wb").write(ico + b"".join(images))
open("gesso-256.png", "wb").write(images[-1])
open("gesso-32.png", "wb").write(images[SIZES.index(32)])

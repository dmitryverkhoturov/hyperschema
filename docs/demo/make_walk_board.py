"""Builds docs/demo/walk.hyperschema: a mind map laid out as a radial tree,
the board the path-mode demo walks over.

    python docs/demo/make_walk_board.py
"""
import json
import math
import os

TREE = ("Put Hyperschema on GitHub", [
    ("README", [
        ("What it is, in one line", []),
        ("Screenshots", []),
        ("Two videos", [
            ("Flight over 100k notes", [
                ("Board generator that looks real", []),
                ("Measure fps", []),
                ("Record at 60 fps", []),
            ]),
            ("Walking the paths", [
                ("A big tree board", []),
                ("Slower slides", []),
                ("Walks are remembered", []),
            ]),
        ]),
        ("How to build", []),
        ("Licence", []),
    ]),
    ("Benchmarks", []),
    ("Releases", [
        ("Windows build", []),
        ("Android APK", []),
        ("Changelog", []),
        ("Version numbers", []),
    ]),
    ("Feedback", [
        ("Issue template", []),
        ("Discussions", []),
        ("Who to show first", [
            ("Friends from the course", []),
            ("Obsidian forum", []),
            ("r/rust", []),
        ]),
    ]),
    ("Clean up", [
        ("No secrets in history", [
            ("Scan every commit", []),
            ("Sync config stays local", []),
        ]),
        ("English everywhere", []),
        ("Remove dev files", []),
    ]),
    ("Next", [
        ("Links to Obsidian notes", []),
        ("Projections", []),
        ("iOS", []),
    ]),
])

# Frames: concepts nested inside, each level several times smaller, as on
# a hand-made board. In path mode they come out at one scale.
FRAMES = {
    "Benchmarks": ["Old laptop: HD 4400 + GeForce 840M", "Load time", "Frame time, p99", "Compare with Excalidraw"],
    "Frame time, p99": ["60 Hz screen: 16.7 ms a frame", "p50 17 ms", "p99 under 25 ms", "Text rebuilt off the frame"],
}

CROSS = [
    ("Measure fps", "Frame time, p99"),
    ("Compare with Excalidraw", "Flight over 100k notes"),
    ("Changelog", "Version numbers"),
    ("Walks are remembered", "Projections"),
    ("Screenshots", "Record at 60 fps"),
    ("Sync config stays local", "Android APK"),
]

FILLS = ["#a5d8ff", "#b2f2bb", "#ffc9c9", "#e5dbff", "#ffd8a8", "#96f2d7"]
ROOT_FILL = "#ffec99"


def leaves(t):
    return 1 if not t[1] else sum(leaves(c) for c in t[1])


nodes, edges = [], []
ids = {}


def size(text, font):
    w = max(font * 7, min(len(text) * font * 0.56 + font * 2, font * 12))
    per_line = (w - font) / (font * 0.56)
    lines = max(1, math.ceil(len(text) / per_line))
    return w, lines * font * 1.25 + font * 1.3


def frame(text, font, fill, x, y):
    """A frame at (x, y) holding FRAMES[text] in rows at about a third of its font."""
    kids = FRAMES[text]
    f = font / 3.2
    pad, gap = font * 0.5, f * 2
    sizes = [frame_size(k, f) if k in FRAMES else size(k, f) for k in kids]
    cols = 2
    col_w = [max(sizes[i][0] for i in range(c, len(kids), cols)) for c in range(cols)]
    rows = [max(sizes[i][1] for i in range(r * cols, min(len(kids), r * cols + cols))) for r in range((len(kids) + cols - 1) // cols)]
    title = font * 1.25 + font
    w = sum(col_w) + gap * (cols - 1) + 2 * pad + gap
    h = title + sum(rows) + gap * (len(rows) - 1) + pad + gap * 0.5
    nid = add(text, x, y, w, h, font, None)
    for i, k in enumerate(kids):
        c, r = i % cols, i // cols
        kx = x + pad + gap * 0.5 + sum(col_w[:c]) + gap * c
        ky = y + title + sum(rows[:r]) + gap * r
        if k in FRAMES:
            kid = frame(k, f, fill, kx, ky)
        else:
            kw, kh = sizes[i]
            kid = add(k, kx, ky, kw, kh, f, fill)
        edges.append({"id": f"e{len(edges):03d}", "fromNode": nid, "toNode": kid, "kind": "contains",
                      "fromEnd": "none", "toEnd": "none"})
    return nid


def frame_size(text, font):
    # Measured by laying it out on a scratch list.
    global nodes, edges
    saved = (nodes, edges, dict(ids))
    nodes, edges = [], []
    frame(text, font, None, 0, 0)
    w, h = nodes[0]["width"], nodes[0]["height"]
    nodes, edges = saved[0], saved[1]
    ids.clear()
    ids.update(saved[2])
    return w, h


def add(text, x, y, w, h, font, fill):
    nid = f"w{len(nodes):03d}"
    ids[text] = nid
    node = {"id": nid, "type": "text", "x": round(x, 1), "y": round(y, 1),
            "width": round(w, 1), "height": round(h, 1), "text": text, "fontSize": round(font, 2)}
    if fill:
        node["fill"] = fill
    nodes.append(node)
    return nid


def place(t, depth, a0, a1, fill):
    text, kids = t
    radius = [0, 340, 640, 920, 1180][depth]
    a = (a0 + a1) / 2
    font = [34, 26, 21, 19, 19][depth]
    cx, cy = radius * math.cos(a) * 1.25, radius * math.sin(a)
    if text in FRAMES:
        w, h = frame_size(text, font)
        nid = frame(text, font, fill, cx - w / 2, cy - h / 2)
    else:
        w, h = size(text, font)
        nid = add(text, cx - w / 2, cy - h / 2, w, h, font, ROOT_FILL if depth == 0 else fill if depth <= 2 else None)
    total = leaves(t)
    at = a0
    for i, k in enumerate(kids):
        span = (a1 - a0) * leaves(k) / total
        f = FILLS[i % len(FILLS)] if depth == 0 else fill
        kid = place(k, depth + 1, at, at + span, f)
        edges.append({"id": f"e{len(edges):03d}", "fromNode": nid, "toNode": kid,
                      "fromEnd": "none", "toEnd": "arrow"})
        at += span
    return nid


place(TREE, 0, -math.pi / 2, 1.5 * math.pi, None)
for a, b in CROSS:
    edges.append({"id": f"e{len(edges):03d}", "fromNode": ids[a], "toNode": ids[b],
                  "fromEnd": "none", "toEnd": "none"})

out = os.path.join(os.path.dirname(os.path.abspath(__file__)), "walk.hyperschema")
with open(out, "w", encoding="utf-8") as f:
    json.dump({"hyperschema": 1, "nodes": nodes, "edges": edges,
               "dimensions": [{"id": "main", "name": "Main", "nest": "contains"}]}, f, ensure_ascii=False, indent=1)
print(len(nodes), "concepts,", len(edges), "links ->", out)

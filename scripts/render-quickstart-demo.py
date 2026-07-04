#!/usr/bin/env python3
"""Render the README quickstart motion demo GIF.

Requires Pillow:

    python3 -m pip install pillow
    scripts/render-quickstart-demo.py assets/open-kioku-quickstart.gif
"""

from __future__ import annotations

import sys
from pathlib import Path

try:
    from PIL import Image, ImageDraw, ImageFont
except ImportError as exc:  # pragma: no cover - developer convenience path
    raise SystemExit(
        "Pillow is required. Install with: python3 -m pip install pillow"
    ) from exc


W, H = 1200, 675
BG = "#0b1020"
PANEL = "#111827"
BORDER = "#334155"
TEXT = "#dbeafe"
MUTED = "#94a3b8"
GREEN = "#86efac"
CYAN = "#67e8f9"
YELLOW = "#fde68a"
RED = "#fca5a5"


def font(size: int, bold: bool = False) -> ImageFont.FreeTypeFont:
    candidates = [
        "/System/Library/Fonts/Menlo.ttc",
        "/Library/Fonts/Arial Unicode.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSansMono-Bold.ttf" if bold else "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
        "/usr/share/fonts/dejavu/DejaVuSansMono-Bold.ttf" if bold else "/usr/share/fonts/dejavu/DejaVuSansMono.ttf",
    ]
    for candidate in candidates:
        if Path(candidate).exists():
            return ImageFont.truetype(candidate, size=size)
    return ImageFont.load_default()


TITLE = font(34, bold=True)
BODY = font(22)
SMALL = font(18)


FRAMES = [
    (
        "Install",
        [
            ("$ npm install -g open-kioku", CYAN),
            ("+ installed native ok binary", GREEN),
            ("$ ok --version", CYAN),
            ("ok <version>", TEXT),
        ],
    ),
    (
        "Index your repo",
        [
            ("$ ok index .", CYAN),
            ("Local metadata: .ok/index.sqlite", GREEN),
            ("Local search: .ok/search/tantivy", MUTED),
        ],
    ),
    (
        "Connect your agent",
        [
            ("$ ok mcp install cursor --repo .", CYAN),
            ("Also tested: claude, codex, gemini, opencode", TEXT),
            ("zed, windsurf, trae", TEXT),
            ("Server args include: --read-only", YELLOW),
        ],
    ),
    (
        "Paste the prompt",
        [
            ("Use Open Kioku before editing.", GREEN),
            ("repo_status -> search_code -> get_definition", TEXT),
            ("impact_analysis -> find_tests_for_change", TEXT),
            ("plan_change first, verify_change after", YELLOW),
        ],
    ),
    (
        "Plan from evidence",
        [
            ("$ ok plan \"token\" --format markdown", CYAN),
            ("Primary context, symbols, impact, tests", TEXT),
            ("Confidence caveats are reported explicitly", MUTED),
        ],
    ),
    (
        "Share proof",
        [
            ("$ ok prove . --task \"auth flow\"", CYAN),
            ("Indexed counts, task scores, validation signals", TEXT),
            ("No source snippets in the report", GREEN),
        ],
    ),
]


def rounded(draw: ImageDraw.ImageDraw, box, radius, fill, outline=None, width=1):
    draw.rounded_rectangle(box, radius=radius, fill=fill, outline=outline, width=width)


def draw_frame(title: str, lines: list[tuple[str, str]], step: int, total: int) -> Image.Image:
    image = Image.new("RGB", (W, H), BG)
    draw = ImageDraw.Draw(image)
    rounded(draw, (44, 40, W - 44, H - 42), 18, PANEL, BORDER, 2)
    draw.ellipse((72, 68, 88, 84), fill=RED)
    draw.ellipse((100, 68, 116, 84), fill=YELLOW)
    draw.ellipse((128, 68, 144, 84), fill=GREEN)
    draw.text((74, 122), "Open Kioku first-win workflow", font=TITLE, fill=TEXT)
    draw.text((76, 174), title, font=BODY, fill=GREEN)
    y = 238
    for line, color in lines:
        draw.text((88, y), line, font=SMALL, fill=color)
        y += 46
    draw.text((76, H - 112), "Plan before edit. Verify after edit.", font=BODY, fill=TEXT)
    bar_x, bar_y, bar_w = 76, H - 72, W - 152
    rounded(draw, (bar_x, bar_y, bar_x + bar_w, bar_y + 12), 6, "#1e293b")
    rounded(draw, (bar_x, bar_y, bar_x + int(bar_w * step / total), bar_y + 12), 6, GREEN)
    draw.text((W - 174, H - 108), f"{step}/{total}", font=SMALL, fill=MUTED)
    return image


def main() -> int:
    out = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("assets/open-kioku-quickstart.gif")
    out.parent.mkdir(parents=True, exist_ok=True)
    frames = [draw_frame(title, lines, idx + 1, len(FRAMES)) for idx, (title, lines) in enumerate(FRAMES)]
    frames[0].save(
        out,
        save_all=True,
        append_images=frames[1:],
        duration=[1150, 1250, 1700, 1350, 1750, 1500],
        loop=0,
        optimize=True,
    )
    print(out)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

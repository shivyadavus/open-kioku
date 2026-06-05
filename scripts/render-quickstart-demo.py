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
        "Create a demo repo",
        [
            ("$ ok demo --force", CYAN),
            ("Indexed demo repo with SQLite + Tantivy", GREEN),
            ("src/auth.rs  tests/auth_flow.rs  ok.toml", MUTED),
        ],
    ),
    (
        "Ask for a plan",
        [
            ("$ ok --repo ./open-kioku-demo plan token --format markdown", CYAN),
            ("Primary context: src/auth.rs", TEXT),
            ("Impact: tests/auth_flow.rs", TEXT),
            ("Validation: cargo test auth_flow", YELLOW),
        ],
    ),
    (
        "Inspect evidence",
        [
            ("Confidence: grounded", GREEN),
            ("Evidence: indexed symbols, tests, boundaries", TEXT),
            ("Negative evidence: exact references not available", MUTED),
        ],
    ),
    (
        "Verify the edit",
        [
            ("$ ok --repo ./open-kioku-demo --json verify --plan /tmp/ok-plan.json --changed src/auth.rs", CYAN),
            ('"verdict": "pass"', GREEN),
            ('"changed_symbols": ["issue_token"]', TEXT),
        ],
    ),
    (
        "Ready for the agent",
        [
            ("search_code -> get_definition -> impact_analysis", TEXT),
            ("find_tests_for_change -> plan_change -> verify_change", TEXT),
            ("Local facts first. No hosted index. No source upload.", GREEN),
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
    draw.text((74, 122), "Open Kioku 60-second quickstart", font=TITLE, fill=TEXT)
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

"""Bake active Lordicon Lottie JSON files into clean monochrome SVG frames for GPUI.

Usage:
    python scripts/bake_lottie_icons.py

Requires:
    - Google Chrome installed at standard Windows path
    - lottie-web installed in scratch dir or LOTTIE_JS env var pointing to lottie.min.js
"""

from __future__ import annotations

import base64
import functools
import http.server
import json
import math
import os
import pathlib
import re
import socketserver
import subprocess
import sys
import threading
import urllib.parse
import xml.dom.minidom as minidom

REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent
ACTIVE_LOTTIE_DIR = REPO_ROOT / "crates" / "ikore-ui" / "assets" / "icons" / "lottie" / "active"
OUT_ANIM_DIR = REPO_ROOT / "crates" / "ikore-ui" / "assets" / "icons" / "anim"

DEFAULT_LOTTIE_JS = pathlib.Path(
    r"C:\Users\natha\.gemini\antigravity\brain\15da89dd-f1b1-4f28-9db7-a7a68099d653\scratch\lottie-baker\node_modules\lottie-web\build\player\lottie.min.js"
)
DEFAULT_PREVIEW_PNG = pathlib.Path(
    r"C:\Users\natha\.gemini\antigravity\brain\15da89dd-f1b1-4f28-9db7-a7a68099d653\scratch\baked_preview.png"
)

CHROME_CANDIDATES = [
    pathlib.Path(r"C:\Program Files\Google\Chrome\Application\chrome.exe"),
    pathlib.Path(r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe"),
    pathlib.Path(r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe"),
    pathlib.Path(r"C:\Program Files\Microsoft\Edge\Application\msedge.exe"),
]

# (clip_slug, source_json_filename, frame_count, mode, scale_wired_stroke)
CLIPS = [
    ("ikore-brand", "ikore-brand-sphere.json", 16, "loop", True),
    ("status-saved", "status-saved-check-circle.json", 16, "reveal", False),
    ("status-loading", "status-loading-spinner-half-circles.json", 16, "loop", False),
    ("status-idle", "status-idle-moon-stars.json", 16, "hover", True),
    ("status-attention", "status-attention-eye.json", 14, "hover", False),
    ("status-error", "status-error-warning-triangle.json", 16, "hover", False),
    ("info-tip", "info-tip-info-circle.json", 16, "hover", False),
    ("nav-home", "nav-home.json", 16, "hover", False),
    ("nav-new-work", "nav-new-work-plus-circle.json", 16, "hover", True),
    ("nav-projects", "nav-projects-folder.json", 16, "hover", False),
    ("nav-pulsations", "nav-pulsations-heart-beat.json", 16, "hover", True),
    ("nav-logs", "nav-logs-file-text.json", 16, "hover", False),
    ("nav-chat", "nav-chat-messages.json", 16, "hover", True),
    ("nav-notes", "nav-notes-file-pencil.json", 16, "hover", True),
    ("nav-canvas", "nav-canvas-layers.json", 16, "hover", False),
    ("nav-workspace", "nav-workspace-code.json", 16, "hover", False),
    ("header-health", "header-health-shield-heart.json", 16, "hover", False),
]

WIRED_STROKE_FACTOR = 2.15


def scale_wired_strokes(obj: object, factor: float) -> None:
    """Multiply non-zero Lottie stroke widths (`ty == 'st'`) by `factor`."""
    if isinstance(obj, dict):
        if obj.get("ty") == "st":
            w = obj.get("w")
            if isinstance(w, dict):
                k = w.get("k")
                if isinstance(k, (int, float)):
                    if k > 0:
                        w["k"] = round(k * factor, 3)
                elif isinstance(k, list):
                    for kf in k:
                        if not isinstance(kf, dict):
                            continue
                        for key in ("s", "e"):
                            val = kf.get(key)
                            if isinstance(val, list):
                                kf[key] = [
                                    round(v * factor, 3) if isinstance(v, (int, float)) and v > 0 else v
                                    for v in val
                                ]
                            elif isinstance(val, (int, float)) and val > 0:
                                kf[key] = round(val * factor, 3)
        for v in obj.values():
            scale_wired_strokes(v, factor)
    elif isinstance(obj, list):
        for item in obj:
            scale_wired_strokes(item, factor)


def load_preprocessed_clips() -> list[dict]:
    jobs = []
    for slug, filename, count, mode, scale_wired in CLIPS:
        src_path = ACTIVE_LOTTIE_DIR / filename
        if not src_path.exists():
            raise FileNotFoundError(f"Missing Lottie JSON: {src_path}")
        data = json.loads(src_path.read_text(encoding="utf-8"))
        if scale_wired and data.get("w", 0) == 430:
            scale_wired_strokes(data, WIRED_STROKE_FACTOR)
        jobs.append(
            {
                "slug": slug,
                "count": count,
                "mode": mode,
                "data": data,
            }
        )
    return jobs


def is_hidden_node(node: minidom.Element) -> bool:
    if node.getAttribute("display") == "none":
        return True
    style = node.getAttribute("style")
    if style and re.search(r"display\s*:\s*none", style, re.I):
        return True
    return False


def normalize_mask_colors(node: minidom.Element) -> None:
    """Normalize non-black matte fills/strokes inside <mask> to #ffffff for usvg luminance masks."""
    for attr in ("fill", "stroke"):
        val = node.getAttribute(attr).strip()
        if not val or val.lower() in ("none", "transparent"):
            continue
        low = val.lower().replace(" ", "")
        if low in ("#000", "#000000", "black", "rgb(0,0,0)"):
            node.setAttribute(attr, "#000000")
        else:
            node.setAttribute(attr, "#ffffff")
    for child in list(node.childNodes):
        if child.nodeType == child.ELEMENT_NODE:
            normalize_mask_colors(child)


def clean_svg(raw_svg: str, width: int, height: int) -> str:
    doc = minidom.parseString(raw_svg.encode("utf-8"))
    svg = doc.documentElement

    # Remove display:none nodes and empty comments/whitespace noise.
    def prune(parent: minidom.Node) -> None:
        for child in list(parent.childNodes):
            if child.nodeType == child.ELEMENT_NODE:
                if is_hidden_node(child):
                    parent.removeChild(child)
                else:
                    prune(child)

    prune(svg)

    # Preserve mask-type="alpha" emitted by lottie-web (supported natively by usvg 0.45.1).

    # Clean root <svg> attributes.
    has_xlink = "xlink:href" in raw_svg
    for attr in list(svg.attributes.keys()):
        if attr not in ("xmlns", "viewBox") and not (has_xlink and attr == "xmlns:xlink"):
            svg.removeAttribute(attr)

    svg.setAttribute("xmlns", "http://www.w3.org/2000/svg")
    svg.setAttribute("width", "24")
    svg.setAttribute("height", "24")
    if not svg.getAttribute("viewBox"):
        svg.setAttribute("viewBox", f"0 0 {width} {height}")

    xml_out = svg.toxml()
    # Collapse excessive blank whitespace lines while keeping valid XML.
    xml_out = re.sub(r">\s+<", "><", xml_out)
    return xml_out + "\n"


BAKE_HTML = r"""<!DOCTYPE html>
<html>
<head><meta charset="utf-8"><title>Lottie SVG Baker</title></head>
<body>
<div id="stage" style="width:240px;height:240px;"></div>
<script src="/lottie.min.js"></script>
<script>
async function run() {
  const stage = document.getElementById('stage');
  const serializer = new XMLSerializer();
  const jobs = await fetch('/jobs.json').then(r => r.json());
  const results = {};

  for (const job of jobs) {
    stage.innerHTML = '';
    const data = job.data;
    const ip = Number(data.ip || 0);
    const op = Number(data.op || 0);
    const total = Math.max(1, op - ip);

    const anim = lottie.loadAnimation({
      container: stage,
      renderer: 'svg',
      loop: false,
      autoplay: false,
      animationData: data,
      rendererSettings: {
        preserveAspectRatio: 'xMidYMid meet',
        progressiveLoad: false,
        hideOnTransparent: false
      }
    });

    const frames = [];
    for (let i = 0; i < job.count; i++) {
      let frame;
      if (job.mode === 'loop') {
        frame = ip + (total * i) / job.count;
      } else {
        const span = Math.max(0, total - 1);
        frame = job.count > 1 ? ip + (span * i) / (job.count - 1) : ip;
      }
      anim.goToAndStop(frame, true);
      const svgEl = stage.querySelector('svg');
      if (!svgEl) {
        throw new Error('No SVG rendered for ' + job.slug + ' frame ' + i);
      }
      frames.push(serializer.serializeToString(svgEl));
    }
    anim.destroy();
    results[job.slug] = { w: data.w, h: data.h, frames };
  }

  await fetch('/submit', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(results)
  });
}

run().catch(async (err) => {
  await fetch('/error', {
    method: 'POST',
    headers: { 'Content-Type': 'text/plain' },
    body: String(err && err.stack ? err.stack : err)
  });
});
</script>
</body>
</html>
"""


def find_browser() -> pathlib.Path:
    for candidate in CHROME_CANDIDATES:
        if candidate.exists():
            return candidate
    raise FileNotFoundError("Chrome/Edge not found for headless SVG baking.")


def append_status_saved_checkmark(svg_text: str, idx: int) -> str:
    """Append progressive checkmark draw inside the 240x240 status-saved circle for idx >= 6."""
    if idx < 6:
        return svg_text
    u = max(0.0, min(1.0, (idx - 5) / 10.0))
    eased = 1.0 - (1.0 - u) ** 2
    p0 = (78.0, 122.0)
    p1 = (106.0, 150.0)
    p2 = (164.0, 92.0)
    l1 = math.hypot(p1[0] - p0[0], p1[1] - p0[1])
    l2 = math.hypot(p2[0] - p1[0], p2[1] - p1[1])
    dist = eased * (l1 + l2)
    if dist <= 0.0:
        return svg_text
    if dist <= l1:
        t = dist / l1
        x = p0[0] + (p1[0] - p0[0]) * t
        y = p0[1] + (p1[1] - p0[1]) * t
        d = f"M{p0[0]:g},{p0[1]:g} L{x:.2f},{y:.2f}"
    else:
        t = min(1.0, (dist - l1) / l2)
        x = p1[0] + (p2[0] - p1[0]) * t
        y = p1[1] + (p2[1] - p1[1]) * t
        d = f"M{p0[0]:g},{p0[1]:g} L{p1[0]:g},{p1[1]:g} L{x:.2f},{y:.2f}"
    check_path = (
        f'<path fill="none" stroke="rgb(18,19,48)" stroke-width="16" '
        f'stroke-linecap="round" stroke-linejoin="round" d="{d}"/>'
    )
    return svg_text.replace("</svg>", f"{check_path}</svg>", 1)


def generate_preview_png(browser: pathlib.Path, baked: dict[str, list[str]], out_png: pathlib.Path) -> None:
    """Render a dark-theme contact sheet PNG (frame 0, mid, last) via headless Chrome."""
    rows = []
    for slug, _, _, mode, _ in CLIPS:
        frames = baked[slug]
        idxs = [0, len(frames) // 2, len(frames) - 1]
        cells = []
        for idx in idxs:
            b64 = base64.b64encode(frames[idx].encode("utf-8")).decode("ascii")
            img_tag = (
                f'<img src="data:image/svg+xml;base64,{b64}" width="26" height="26" '
                f'style="filter: brightness(0) invert(1);" />'
            )
            cells.append(
                f'<div style="display:flex;flex-direction:column;align-items:center;gap:4px;">'
                f'<div style="width:40px;height:40px;background:#0d1117;border:1px solid #1d242e;border-radius:8px;display:flex;align-items:center;justify-content:center;">'
                f'{img_tag}'
                f'</div><span style="font-size:10px;color:#8b97a7;">f{idx:02d}</span></div>'
            )
        rows.append(
            f'<div style="background:#151b23;border:1px solid #1d242e;border-radius:10px;padding:10px;display:flex;align-items:center;justify-content:space-between;gap:10px;">'
            f'<div><div style="font-size:12px;font-weight:600;color:#e6ebf1;">{slug}</div>'
            f'<div style="font-size:10px;color:#73b7ff;">{mode} ({len(frames)}f)</div></div>'
            f'<div style="display:flex;gap:6px;">{"".join(cells)}</div></div>'
        )

    html = (
        '<!DOCTYPE html><html><body style="margin:0;padding:16px;background:#06080b;font-family:Segoe UI,sans-serif;">'
        '<h2 style="margin:0 0 12px 0;font-size:15px;color:#22d3ee;">Ikore Baked SVG Contact Sheet (f00 / mid / end)</h2>'
        f'<div style="display:grid;grid-template-columns:repeat(3, 1fr);gap:10px;">{"".join(rows)}</div>'
        '</body></html>'
    )
    out_png.parent.mkdir(parents=True, exist_ok=True)
    preview_html = out_png.with_suffix(".html")
    preview_html.write_text(html, encoding="utf-8")
    subprocess.run(
        [
            str(browser),
            "--headless=new",
            "--disable-gpu",
            "--no-sandbox",
            "--window-size=1100,980",
            f"--screenshot={out_png}",
            preview_html.as_uri(),
        ],
        check=True,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    preview_html.unlink(missing_ok=True)


def main() -> int:
    lottie_js = pathlib.Path(os.environ.get("LOTTIE_JS", str(DEFAULT_LOTTIE_JS)))
    if not lottie_js.exists():
        print(f"Missing lottie.min.js at {lottie_js}", file=sys.stderr)
        return 1

    browser = find_browser()
    jobs = load_preprocessed_clips()
    jobs_bytes = json.dumps(jobs).encode("utf-8")
    lottie_bytes = lottie_js.read_bytes()

    state: dict[str, object] = {"results": None, "error": None}
    done_evt = threading.Event()

    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, format: str, *args: object) -> None:
            return

        def do_GET(self) -> None:
            path = urllib.parse.urlparse(self.path).path
            if path == "/bake.html":
                body = BAKE_HTML.encode("utf-8")
                ctype = "text/html; charset=utf-8"
            elif path == "/lottie.min.js":
                body = lottie_bytes
                ctype = "application/javascript; charset=utf-8"
            elif path == "/jobs.json":
                body = jobs_bytes
                ctype = "application/json; charset=utf-8"
            else:
                self.send_response(404)
                self.end_headers()
                return
            self.send_response(200)
            self.send_header("Content-Type", ctype)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def do_POST(self) -> None:
            length = int(self.headers.get("Content-Length", "0"))
            payload = self.rfile.read(length)
            if self.path == "/submit":
                state["results"] = json.loads(payload.decode("utf-8"))
                self.send_response(200)
                self.end_headers()
                done_evt.set()
            elif self.path == "/error":
                state["error"] = payload.decode("utf-8", errors="replace")
                self.send_response(200)
                self.end_headers()
                done_evt.set()
            else:
                self.send_response(404)
                self.end_headers()

    class ReusableServer(socketserver.ThreadingMixIn, http.server.HTTPServer):
        daemon_threads = True

    with ReusableServer(("127.0.0.1", 0), Handler) as srv:
        port = srv.server_address[1]
        thread = threading.Thread(target=srv.serve_forever, daemon=True)
        thread.start()

        proc = subprocess.Popen(
            [
                str(browser),
                "--headless=new",
                "--disable-gpu",
                "--no-sandbox",
                "--window-size=400,400",
                f"http://127.0.0.1:{port}/bake.html",
            ],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        finished = done_evt.wait(timeout=60)
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
        srv.shutdown()

    if not finished:
        print("Timed out waiting for headless browser bake.", file=sys.stderr)
        return 1
    if state["error"] is not None:
        print(f"Browser bake error: {state['error']}", file=sys.stderr)
        return 1

    results = state["results"]
    assert isinstance(results, dict)

    OUT_ANIM_DIR.mkdir(parents=True, exist_ok=True)
    baked_svgs: dict[str, list[str]] = {}
    total_files = 0
    for slug, _, _, _, _ in CLIPS:
        entry = results[slug]
        w, h = int(entry["w"]), int(entry["h"])
        clip_dir = OUT_ANIM_DIR / slug
        clip_dir.mkdir(parents=True, exist_ok=True)
        for old in clip_dir.glob("*.svg"):
            old.unlink()
        cleaned_frames = []
        for idx, raw in enumerate(entry["frames"]):
            cleaned = clean_svg(raw, w, h)
            if slug == "status-saved":
                cleaned = append_status_saved_checkmark(cleaned, idx)
            (clip_dir / f"f{idx:02d}.svg").write_text(cleaned, encoding="utf-8")
            cleaned_frames.append(cleaned)
            total_files += 1
        baked_svgs[slug] = cleaned_frames

    preview_path = pathlib.Path(os.environ.get("BAKE_PREVIEW_PNG", str(DEFAULT_PREVIEW_PNG)))
    generate_preview_png(browser, baked_svgs, preview_path)
    print(f"Baked {len(CLIPS)} clips ({total_files} SVG files) into {OUT_ANIM_DIR}")
    print(f"Preview contact sheet written to {preview_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

#!/usr/bin/env python3
"""Diagnose the inline Hover Card demo.

Boots the demo, prints:
  - Post-parse outerHTML of the HoverCardDemo component (so we can
    see if `<p>` auto-closed or any other parser fix-up ruined the
    nesting).
  - Computed display + position of every `<pine-hover-card-*>` tag
    plus the surrounding text nodes.
  - Bounding boxes — so we can tell if the sentence wrapped onto
    one line or fragmented into stacked blocks.

Usage:
    # one-time (if not already set up)
    python3 -m pip install --user --break-system-packages playwright
    python3 -m playwright install firefox
    # rebuild wasm
    cargo run -p pocopine-cli -- build --path examples/website
    # run
    python3 examples/website/e2e/capture_hover_card.py
"""

from __future__ import annotations

import contextlib
import http.server
import json
import socketserver
import sys
import threading
from pathlib import Path

from playwright.sync_api import sync_playwright


DEMO_ROOT = Path(__file__).resolve().parent.parent


class _QuietHandler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *_a, **_kw):
        pass


class _ReusableTCPServer(socketserver.TCPServer):
    allow_reuse_address = True


@contextlib.contextmanager
def _serve():
    handler = lambda *a, **kw: _QuietHandler(*a, directory=str(DEMO_ROOT), **kw)
    server = _ReusableTCPServer(("127.0.0.1", 0), handler)
    port = server.server_address[1]
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield f"http://127.0.0.1:{port}/"
    finally:
        server.shutdown()
        server.server_close()


def main() -> int:
    with _serve() as base_url, sync_playwright() as pw:
        browser = pw.firefox.launch()
        page = browser.new_page(viewport={"width": 820, "height": 900})
        page.goto(base_url, wait_until="networkidle")
        page.wait_for_selector("hover-card-demo", timeout=5000)

        demo = page.locator("hover-card-demo")

        print("=" * 70)
        print("outerHTML (first 2 KB — looks for <p>/<div> drift):")
        print("=" * 70)
        html = demo.evaluate("el => el.outerHTML")
        print(html[:2000])
        print()

        print("=" * 70)
        print("Computed display for every pine-hover-card-* tag:")
        print("=" * 70)
        rows = demo.evaluate(
            """el => {
                const out = [];
                el.querySelectorAll('*').forEach(node => {
                    const tag = node.tagName.toLowerCase();
                    if (!tag.startsWith('pine-hover-card-')) return;
                    const cs = getComputedStyle(node);
                    const r = node.getBoundingClientRect();
                    out.push({
                        tag,
                        display: cs.display,
                        position: cs.position,
                        width: Math.round(r.width),
                        height: Math.round(r.height),
                        x: Math.round(r.x),
                        y: Math.round(r.y),
                    });
                });
                return out;
            }"""
        )
        for r in rows:
            print(f"  {r}")
        print()

        print("=" * 70)
        print("Text-flow check — top coordinates of every @mention link")
        print("(all should share the same Y if the sentence is one line;")
        print(" otherwise wrapping put them on different baselines).")
        print("=" * 70)
        mentions = demo.evaluate(
            """el => Array.from(el.querySelectorAll('.hc-link')).map(a => {
                const r = a.getBoundingClientRect();
                return {
                    text: a.textContent.trim(),
                    y: Math.round(r.y),
                    x: Math.round(r.x),
                    inline_parent_tag: a.parentElement.parentElement.tagName.toLowerCase(),
                };
            })"""
        )
        for m in mentions:
            print(f"  {m}")
        print()

        print("=" * 70)
        print("Parent of hc-prose wrapper + its first few children")
        print("(so we can see if the browser un-nested anything):")
        print("=" * 70)
        info = demo.evaluate(
            """el => {
                const p = el.querySelector('.hc-prose');
                if (!p) return { found: false };
                const kids = Array.from(p.childNodes).slice(0, 10).map(n => ({
                    node_type: n.nodeType,
                    tag: n.nodeType === 1 ? n.tagName.toLowerCase() : null,
                    text: n.nodeType === 3 ? n.textContent.trim() : null,
                }));
                return {
                    found: true,
                    wrapper_tag: p.tagName.toLowerCase(),
                    display: getComputedStyle(p).display,
                    child_count: p.childNodes.length,
                    first_children: kids,
                };
            }"""
        )
        print(json.dumps(info, indent=2))
        print()

        out_png = DEMO_ROOT / "e2e" / "hover_card_capture.png"
        demo.screenshot(path=str(out_png))
        print(f"Screenshot written to {out_png}")

        browser.close()
        return 0


if __name__ == "__main__":
    sys.exit(main())

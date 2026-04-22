#!/usr/bin/env python3
"""Smoke-check pine-icons integration in the site header.

Boots the demo, verifies:
  - <pine-icon name="search">   renders a real <svg> in the header search button.
  - <pine-icon :name="…">       reacts to theme toggle (swaps moon↔sun SVG).

Usage:
    wasm-pack build --target web --out-dir pkg --dev examples/website
    python3 examples/website/e2e/capture_header_icons.py
"""

from __future__ import annotations

import contextlib
import http.server
import socketserver
import sys
import threading
from pathlib import Path

from playwright.sync_api import sync_playwright


DEMO_ROOT = Path(__file__).resolve().parent.parent


class _Q(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *_a, **_kw):
        pass


class _R(socketserver.TCPServer):
    allow_reuse_address = True


@contextlib.contextmanager
def _serve():
    handler = lambda *a, **kw: _Q(*a, directory=str(DEMO_ROOT), **kw)
    server = _R(("127.0.0.1", 0), handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield f"http://127.0.0.1:{server.server_address[1]}/"
    finally:
        server.shutdown()
        server.server_close()


def main() -> int:
    with _serve() as base, sync_playwright() as pw:
        browser = pw.firefox.launch()
        page = browser.new_page(viewport={"width": 900, "height": 700})
        page.goto(base, wait_until="networkidle")
        # `display: contents` on the custom tag means Playwright's
        # default visibility check fails. Wait for presence instead.
        page.wait_for_selector("site-header pine-icon", state="attached", timeout=5000)

        search = page.locator("site-header .site-header-search pine-icon")
        theme_icon = page.locator("site-header .site-header-theme pine-icon")

        search_svg_count = search.evaluate("el => el.querySelectorAll('svg').length")
        theme_svg_count = theme_icon.evaluate("el => el.querySelectorAll('svg').length")
        print(f"search <svg> count:     {search_svg_count}")
        print(f"theme toggle <svg> count: {theme_svg_count}")
        print("---- search icon outerHTML ----")
        print(search.evaluate("el => el.outerHTML"))
        print("---- theme icon outerHTML ----")
        print(theme_icon.evaluate("el => el.outerHTML"))
        assert search_svg_count == 1, "search icon didn't render an <svg>"
        assert theme_svg_count == 1, "theme icon didn't render an <svg>"

        before = theme_icon.evaluate("el => el.innerHTML")
        page.locator(".site-header-theme").click()
        page.wait_for_timeout(150)
        after = theme_icon.evaluate("el => el.innerHTML")
        assert before != after, "theme toggle didn't change the icon SVG"
        print("theme toggle flipped the icon SVG: ok")

        page.locator("site-header").screenshot(
            path=str(DEMO_ROOT / "e2e" / "header_icons.png")
        )
        print(f"screenshot: {DEMO_ROOT / 'e2e' / 'header_icons.png'}")
        browser.close()
        return 0


if __name__ == "__main__":
    sys.exit(main())

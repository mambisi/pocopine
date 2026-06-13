#!/usr/bin/env python3
"""Runtime smoke + screenshots for the rebranded, routed docs site.

Walks the site the way a user would — clicking through nav, a
component card (client-routed via the click delegate), the Code tab,
and a docs sidebar link.

    cargo run -p pocopine-cli -- build --path examples/website
    python3 examples/website/e2e/capture_rebrand.py
"""

from __future__ import annotations

import http.server
import socketserver
import threading
from pathlib import Path

from playwright.sync_api import sync_playwright

ROOT = Path(__file__).resolve().parent.parent
SHOTS = ROOT / "e2e"


class Q(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *a, **k):
        pass

    def do_GET(self):
        p = self.path.split("?")[0]
        if not (ROOT / p.lstrip("/")).exists() and "." not in Path(p).name:
            self.path = "/index.html"
        # `pocopine build` writes the hash-rewritten entry point to
        # pkg/index.html (the source index.html keeps the stable
        # unhashed bundle reference); prefer the generated copy like
        # pocopine's real servers do.
        if self.path in ("/", "/index.html") and (ROOT / "pkg" / "index.html").is_file():
            self.path = "/pkg/index.html"
        return super().do_GET()


class R(socketserver.TCPServer):
    allow_reuse_address = True


def main() -> int:
    srv = R(("127.0.0.1", 0), lambda *a, **k: Q(*a, directory=str(ROOT), **k))
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    base = f"http://127.0.0.1:{srv.server_address[1]}"

    with sync_playwright() as pw:
        b = pw.firefox.launch()
        pg = b.new_page(viewport={"width": 1280, "height": 900})
        errs: list[str] = []
        pg.on("console", lambda m: errs.append(m.text) if m.type == "error" else None)

        def shot(n):
            pg.wait_for_timeout(450)
            pg.screenshot(path=str(SHOTS / f"rb_{n}.png"), full_page=True)

        pg.goto(base + "/", wait_until="networkidle")
        pg.wait_for_selector(".landing .hero", timeout=15000)
        shot("landing")

        # Header nav → Components index
        pg.click(".site-nav a:has-text('Components')")
        pg.wait_for_selector(".ci-card", timeout=10000)
        pg.wait_for_timeout(400)
        card = pg.locator(".ci-card").first
        print(f"[components] cards={pg.locator('.ci-card').count()} "
              f"first={card.inner_text()!r} href={card.get_attribute('href')!r}")
        shot("components")

        # Click a card (client-routed via delegate, no pp-route)
        pg.click(".ci-card:has-text('Dialog')")
        pg.wait_for_selector(".component-page", timeout=10000)
        pg.wait_for_timeout(400)
        print(f"[detail] url={pg.url} preview_hosts={pg.locator('.cp-preview .cp-preview-host').count()} "
              f"prop_rows={pg.locator('.props-table tbody tr').count()}")
        shot("component_preview")
        pg.click(".cp-tab:has-text('Code')")
        pg.wait_for_timeout(300)
        print(f"[detail] code_chars={len(pg.locator('.cp-code code').inner_text())}")
        shot("component_code")

        # Header nav → Docs
        pg.click(".site-nav a:has-text('Docs')")
        pg.wait_for_selector(".doc-layout", timeout=10000)
        pg.wait_for_timeout(900)
        nav_links = pg.locator(".doc-nav-link[data-header='false']")
        print(f"[docs] url={pg.url} body_chars={len(pg.locator('.doc-body').inner_html())} "
              f"sidebar_links={nav_links.count()} first_link={nav_links.first.inner_text()!r} "
              f"toc={pg.locator('.doc-toc-link').count()}")
        shot("docs")

        # Click a sidebar link (client-routed via delegate)
        pg.click(".doc-nav-link[data-header='false']:has-text('Quickstart')")
        pg.wait_for_timeout(900)
        print(f"[docs nav] url={pg.url} title_h1={pg.locator('.doc-body h1').first.inner_text()!r}")
        shot("docs_quickstart")

        b.close()
        print("\nconsole errors:" if errs else "\nno console errors")
        for e in errs[:20]:
            print("  ", e)
    srv.shutdown()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

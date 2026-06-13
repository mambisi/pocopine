#!/usr/bin/env python3
"""Animation perf baseline — opens the 500-row stress fixture and
shuffles it 10×, recording wall-clock time for each shuffle.

Reports:
  * mean time per shuffle (ms)
  * max time per shuffle (ms)
  * how many `Animation` objects ended up running (FLIP count)

Use to spot regressions in the RFC-039 dispatch + FLIP path. The
target hardware is a developer laptop in headless Firefox; record
the numbers in commit messages to track drift.
"""

from __future__ import annotations

import contextlib
import http.server
import socketserver
import threading
import time
from pathlib import Path

from playwright.sync_api import sync_playwright


DEMO_ROOT = Path(__file__).resolve().parent.parent


class _QuietHandler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *_args, **_kwargs):
        pass

    def send_head(self):
        # `pocopine build` writes the hash-rewritten entry point to
        # pkg/index.html (the source index.html keeps the stable
        # unhashed bundle reference); prefer the generated copy like
        # pocopine's real servers do.
        if self.path in ("/", "/index.html") and (DEMO_ROOT / "pkg" / "index.html").is_file():
            self.path = "/pkg/index.html"
        return super().send_head()


class _ReusableTCPServer(socketserver.TCPServer):
    allow_reuse_address = True


@contextlib.contextmanager
def _serve():
    handler = lambda *a, **kw: _QuietHandler(*a, directory=str(DEMO_ROOT), **kw)
    server = _ReusableTCPServer(("127.0.0.1", 0), handler)
    port = server.server_address[1]
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        yield f"http://127.0.0.1:{port}/"
    finally:
        server.shutdown()
        server.server_close()


SHUFFLE_COUNT = 10


def main() -> int:
    with _serve() as base_url, sync_playwright() as p:
        browser = p.firefox.launch(headless=True)
        page = browser.new_page(viewport={"width": 1100, "height": 900})
        page.on("pageerror", lambda err: print(f"  [PAGE ERROR] {err}"))
        page.on("console", lambda msg: print(f"  [CONSOLE {msg.type}] {msg.text}") if msg.type in ("error", "warning") else None)
        page.goto(base_url)
        page.wait_for_selector("website-app pine-button", timeout=10_000)

        # Mount the stress fixture.
        button = page.locator("pine-button:has-text('Mount 500-row stress fixture')")
        print(f"mount button count: {button.count()}")
        button.first.scroll_into_view_if_needed()
        button.first.click()
        time.sleep(0.5)
        item_count = page.evaluate(
            "() => document.querySelectorAll('pine-tags-input-item').length"
        )
        # Other TagsInput demos contribute ~8 chips; the stress
        # fixture adds 500. Round to the nearest 100 for the check.
        print(f"items after mount click: {item_count}")
        if item_count < 500:
            print("FAILED: expected ≥ 500 items")
            browser.close()
            return 1

        # The stress fixture's Shuffle button is the LAST one on
        # the page (the demo also has 3 other Shuffle buttons for
        # the smaller TagsInput sections). Pick by index.
        shuffle = page.locator("pine-button:has-text('Shuffle')").last
        durations_ms: list[float] = []
        for _ in range(SHUFFLE_COUNT):
            t0 = time.perf_counter()
            shuffle.click()
            # Wait for the FLIP animations to settle (default 260ms +
            # margin) before timing the next shuffle.
            time.sleep(0.40)
            t1 = time.perf_counter()
            durations_ms.append((t1 - t0) * 1000.0)

        # Pull the demo's own measured shuffle ms (just the
        # reactive update; excludes FLIP play + click delivery).
        # The indicator is `<strong pp-text="stress_last_shuffle_ms">`
        # near the bottom of the page.
        in_browser_ms = page.evaluate(
            """() => {
              const el = document.querySelector('strong[pp-text=\"stress_last_shuffle_ms\"]');
              return el ? parseFloat(el.textContent) : null;
            }"""
        )

        running_anims = page.evaluate(
            "() => document.getAnimations ? document.getAnimations().length : -1"
        )

        browser.close()

    mean_ms = sum(durations_ms) / len(durations_ms)
    max_ms = max(durations_ms)
    print(f"shuffles : {SHUFFLE_COUNT}")
    print(f"mean ms  : {mean_ms:.1f}")
    print(f"max ms   : {max_ms:.1f}")
    print(f"in-browser shuffle ms (last): {in_browser_ms}")
    print(f"running animations after settle: {running_anims}")
    print()
    print("Baseline numbers pre-cache (record in PR description).")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

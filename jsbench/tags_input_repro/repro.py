#!/usr/bin/env python3
"""Real-browser reproducer for the tags-input pp-for delete bug.

Serves `jsbench/tags_input_repro/` over a temporary HTTP server,
opens it in chromium via Playwright, and exercises:

  1. The seeded-3-tags case: click middle chip's ×, assert it's
     gone.
  2. The 500-tag case: click "Seed 500", click 250th chip's ×,
     assert it's gone.

Captures any console errors / page errors so we surface a real
runtime failure that the headless wasm-bindgen-test harness
would have hidden.

Usage:
  python3 jsbench/tags_input_repro/repro.py [--browser chromium|firefox]
"""
from __future__ import annotations

import argparse
import contextlib
import http.server
import socketserver
import sys
import threading
from pathlib import Path

from playwright.sync_api import sync_playwright


HARNESS_DIR = Path(__file__).resolve().parent


@contextlib.contextmanager
def serve(directory: Path):
    handler = lambda *a, **kw: http.server.SimpleHTTPRequestHandler(
        *a, directory=str(directory), **kw
    )
    with socketserver.TCPServer(("127.0.0.1", 0), handler) as srv:
        port = srv.server_address[1]
        thread = threading.Thread(target=srv.serve_forever, daemon=True)
        thread.start()
        try:
            yield f"http://127.0.0.1:{port}/"
        finally:
            srv.shutdown()


def run(browser_name: str) -> int:
    failures: list[str] = []
    page_errors: list[str] = []
    console_errors: list[str] = []

    with serve(HARNESS_DIR) as base_url, sync_playwright() as p:
        browser_type = getattr(p, browser_name)
        browser = browser_type.launch(headless=True)
        context = browser.new_context()
        page = context.new_page()
        page.on("pageerror", lambda err: page_errors.append(str(err)))
        all_console: list[str] = []
        page.on(
            "console",
            lambda msg: (
                console_errors.append(msg.text) if msg.type == "error" else None,
                all_console.append(f"[{msg.type}] {msg.text}"),
            ),
        )

        try:
            page.goto(base_url)
            page.wait_for_selector(".chip", timeout=10_000)

            # ── Case 1: 3 chips, click middle delete ──────────
            chips = page.locator(".chip")
            assert chips.count() == 3, f"expected 3 chips, got {chips.count()}"

            # Diagnostic: dump the middle chip's outer HTML so we
            # can see what tag/attrs the click target actually
            # has after compilation.
            outer = page.evaluate(
                "document.querySelectorAll('.chip-x')[1].outerHTML"
            )
            print(f"  middle .chip-x: {outer!r}")
            tag_name = page.evaluate(
                "document.querySelectorAll('.chip-x')[1].tagName"
            )
            print(f"  tagName: {tag_name}")
            # Attach a JS-side listener to see if the click event
            # actually reaches the chip × at all.
            page.evaluate(
                "document.querySelectorAll('.chip-x')[1]"
                ".addEventListener('click', () => "
                "  document.title = '__chip_click_seen__', true);"
            )

            page.locator(".chip-x").nth(1).click()
            page.wait_for_timeout(200)
            print(f"  document.title after click: {page.title()!r}")

            # Diagnostic: walk up from the chip × and report every
            # stamped pp-* marker we find — `__pp_mounted`,
            # SCOPE_ID_KEY, listener id, etc. The compiled mount
            # path stamps these per-element; if any are missing
            # on a chip × that was rendered via pp-for, that's
            # the smoking gun.
            stamps = page.evaluate(
                """
                () => {
                  const x = document.querySelectorAll('.chip-x')[1];
                  const item = x.closest('pine-tags-input-item');
                  const root = x.closest('pine-tags-input-root');
                  const dump = (el, label) => {
                    if (!el) return [label, null];
                    const props = {};
                    for (const k of Object.keys(el)) {
                      if (!k.startsWith('__pp')) continue;
                      const v = el[k];
                      if (typeof v === 'number' || typeof v === 'boolean') props[k] = v;
                      else if (typeof v === 'string') props[k] = v.slice(0, 60);
                      else if (Array.isArray(v)) props[k] = `arr(len=${v.length})`;
                      else props[k] = '<obj>';
                    }
                    return [label, el.tagName, el.className, props];
                  };
                  return {
                    chip_x: dump(x, 'x'),
                    item: dump(item, 'item'),
                    root: dump(root, 'root'),
                  };
                }
                """
            )
            print(f"  stamps: {stamps}")

            chips = page.locator(".chip")
            n = chips.count()
            if n != 2:
                failures.append(
                    f"after clicking middle ×: expected 2 chips, got {n}. "
                    f"State: {page.locator('#state').text_content()}"
                )

            # ── Case 2: seed 500, click 250th delete ──────────
            page.locator("#seed-500").click()
            page.wait_for_function(
                "document.querySelectorAll('.chip').length === 500",
                timeout=15_000,
            )

            page.locator(".chip-x").nth(249).click()
            page.wait_for_timeout(500)

            n_after = page.locator(".chip").count()
            if n_after != 499:
                failures.append(
                    f"after clicking 250th × (500 chips seeded): "
                    f"expected 499 chips, got {n_after}"
                )

        finally:
            browser.close()

    print(f"== tags-input repro — {browser_name} ==")
    print(f"page errors: {len(page_errors)}")
    for err in page_errors:
        print(f"  {err}")
    print(f"console errors: {len(console_errors)}")
    for err in console_errors:
        print(f"  {err}")
    print(f"console msgs (delete-related):")
    for line in all_console:
        if any(k in line for k in ["[delete]", "[error]", "for_::install", "materialize_slot"]):
            print(f"  {line}")
    print(f"failures: {len(failures)}")
    for f in failures:
        print(f"  {f}")

    return 0 if not failures and not page_errors else 1


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--browser",
        default="chromium",
        choices=["chromium", "firefox", "webkit"],
    )
    args = parser.parse_args()
    return run(args.browser)


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""End-to-end checks for RFC-038 animations in the pine-demo.

Verifies:
  * Opening the Dialog plays the `fade-scale` enter — opacity sits
    below 1 mid-animation, settles to 1 once it completes.
  * Closing the Dialog defers DOM removal until the leave animation
    finishes (element still present mid-animation, gone after).
  * Shuffling a TagsInput list applies a non-identity transform to
    at least one chip mid-FLIP.
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


def main() -> int:
    failures: list[str] = []

    with _serve() as base_url, sync_playwright() as p:
        browser = p.firefox.launch(headless=True)
        page = browser.new_page(viewport={"width": 1100, "height": 900})
        page.on("pageerror", lambda err: print(f"  [PAGE ERROR] {err}"))
        page.on("console", lambda msg: print(f"  [CONSOLE {msg.type}] {msg.text}") if msg.type in ("log", "warning", "error") else None)
        page.goto(base_url)
        page.wait_for_selector("pine-demo-app pine-button", timeout=10_000)

        # ── Dialog enter: opacity under 1 mid-animation ───────────
        page.locator("pine-dialog-trigger button").first.click()
        # Sample immediately — before the next animation frame swaps
        # `enter-start` for `enter-end`. At this instant the outer
        # element should carry `*-from` (opacity 0).
        opacity_mid_dump = page.evaluate(
            """() => {
              const outer = document.querySelector('pine-dialog-content');
              const inner = document.querySelector('pine-dialog-content > div');
              const cs = outer ? getComputedStyle(outer) : null;
              return {
                outer_opacity: cs ? cs.opacity : null,
                // The demo's dialog CSS uses the standalone `scale`
                // property (not transform) so `@starting-style` can
                // provide the from-state. Check both.
                outer_scale: cs ? cs.scale : null,
                outer_transform: cs ? cs.transform : null,
                outer_transition: cs ? cs.transition : null,
                outer_classes: outer ? outer.className : null,
                inner_opacity: inner ? getComputedStyle(inner).opacity : null,
              };
            }"""
        )
        print("MID-ANIM SAMPLE:", opacity_mid_dump)
        # The dialog demo uses opacity-only fade via @starting-style
        # (no scale — see styles.css for why). Check opacity is
        # partway through the tween.
        opacity_mid = float(opacity_mid_dump["outer_opacity"] or "1")
        if opacity_mid >= 0.99:
            failures.append(
                f"dialog enter: expected opacity < 1 mid-animation, got {opacity_mid}"
            )
        time.sleep(0.30)
        opacity_after = float(
            page.evaluate(
                "() => getComputedStyle("
                "document.querySelector('pine-dialog-content')"
                ").opacity"
            )
        )
        if opacity_after < 0.99:
            failures.append(
                f"dialog enter end: expected opacity ≈ 1, got {opacity_after}"
            )

        # ── Dialog leave: removal deferred ────────────────────────
        page.locator("pine-dialog-content >> text=Cancel").click()
        time.sleep(0.05)
        still_present = page.evaluate(
            "() => !!document.querySelector('pine-dialog-content')"
        )
        if not still_present:
            failures.append(
                "dialog leave: removed too early — expected to remain mid-animation"
            )
        time.sleep(0.40)
        gone = page.evaluate(
            "() => !document.querySelector('pine-dialog-content')"
        )
        if not gone:
            failures.append(
                "dialog leave: still present after settle — expected removal"
            )

        # ── TagsInput FLIP on shuffle ─────────────────────────────
        page.locator("h2:has-text('Tags input')").first.scroll_into_view_if_needed()
        # Find the "rust" chip's initial position so we can verify it
        # actually moved after the shuffle (rotating the list pushes
        # rust from index 0 to the end).
        rust_initial = page.locator(
            "pine-tags-input-item:has-text('rust')"
        ).first.bounding_box()

        chips_before = page.evaluate(
            "() => Array.from(document.querySelectorAll('pine-tags-input-item'))"
            ".map(el => ({"
            "  text: el.textContent.trim().slice(0, 30),"
            "  data_animate: el.getAttribute('data-pp-animate'),"
            "  classes: el.className,"
            "}))"
        )
        print("CHIPS BEFORE SHUFFLE:")
        for c in chips_before:
            print("  -", c)
        page.locator("pine-button:has-text('Shuffle')").first.click()
        time.sleep(0.10)
        chips_after = page.evaluate(
            "() => Array.from(document.querySelectorAll('pine-tags-input-item'))"
            ".map(el => {"
            "  const inner = el.firstElementChild;"
            "  return {"
            "    text: el.textContent.trim().slice(0, 30),"
            "    transform: inner ? getComputedStyle(inner).transform : 'no-inner',"
            "    data_animate: el.getAttribute('data-pp-animate'),"
            "  };"
            "})"
        )
        print("CHIPS MID-FLIP:")
        for c in chips_after:
            print("  -", c)
        transforms = [c["transform"] for c in chips_after]
        moved = [
            t for t in transforms
            if t and t != "none" and "matrix(1, 0, 0, 1, 0, 0)" not in t
        ]
        if not moved:
            failures.append(
                "FLIP: expected at least one chip with a non-identity transform "
                f"mid-animation; saw {transforms}"
            )

        time.sleep(0.40)
        rust_final = page.locator(
            "pine-tags-input-item:has-text('rust')"
        ).first.bounding_box()
        if (
            rust_initial and rust_final
            and abs(rust_initial["x"] - rust_final["x"]) < 5
            and abs(rust_initial["y"] - rust_final["y"]) < 5
        ):
            failures.append(
                f"FLIP settle: rust chip didn't move after shuffle "
                f"(initial={rust_initial}, final={rust_final})"
            )

        browser.close()

    if failures:
        print("FAIL")
        for f in failures:
            print(" -", f)
        return 1
    print("OK — RFC-038 animations behave end-to-end")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

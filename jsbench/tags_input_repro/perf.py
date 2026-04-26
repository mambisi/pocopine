#!/usr/bin/env python3
"""Time tags-input shuffle + add for the perf-regression diagnosis.

Serves the harness, fills 500 tags, then:
  - clicks Shuffle 5 times, records click→post-settle ms each
  - clicks Add one 5 times, records same

Reports mean/p50/p95 for each. Use to confirm/refute the user-
reported regression (shuffle 70ms → 200-1000ms).
"""
from __future__ import annotations

import contextlib
import http.server
import socketserver
import statistics
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


def time_action(page, button_id: str, expected_count: int) -> float:
    """Click the button, wait for post-action quiescence, return ms."""
    t0 = page.evaluate("performance.now()")
    page.locator(f"#{button_id}").click()
    # Wait until DOM mutations settle: rAF + microtask drain.
    page.evaluate(
        "() => new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r)))"
    )
    t1 = page.evaluate("performance.now()")
    actual = page.locator(".chip").count()
    if actual != expected_count:
        print(f"  warn: expected {expected_count} chips, got {actual}")
    return t1 - t0


def main() -> int:
    with serve(HARNESS_DIR) as base_url, sync_playwright() as p:
        browser = p.chromium.launch(headless=True)
        page = browser.new_context().new_page()
        page.on("pageerror", lambda err: print(f"[ERROR] {err}"))

        page.goto(base_url)
        page.wait_for_selector(".chip", timeout=10_000)

        # Initial 3 chips (matches the website demo's default
        # state). Bench shuffle + add at this scale — what the
        # user actually exercises in the demo.
        # Warm-up + measure shuffles.
        for _ in range(2):
            page.locator("#shuffle").click()
            page.wait_for_timeout(50)
        small_shuffle_times = [time_action(page, "shuffle", 3) for _ in range(8)]

        # Add-one at small scale.
        small_add_times = []
        n = 3
        for _ in range(5):
            n += 1
            small_add_times.append(time_action(page, "add-one", n))

        # Then jump to 500 for stress numbers.
        page.locator("#seed-500").click()
        page.wait_for_function(
            "document.querySelectorAll('.chip').length === 500", timeout=15_000
        )
        for _ in range(2):
            page.locator("#shuffle").click()
            page.wait_for_timeout(100)
        shuffle_times = [time_action(page, "shuffle", 500) for _ in range(5)]
        add_times = []
        n = 500
        for _ in range(5):
            n += 1
            add_times.append(time_action(page, "add-one", n))

        browser.close()

    def fmt(name: str, samples: list[float]):
        m = statistics.mean(samples)
        p50 = statistics.median(samples)
        mx = max(samples)
        print(f"  {name:>16}: mean={m:7.2f}ms  p50={p50:7.2f}ms  max={mx:7.2f}ms  n={len(samples)}")

    print("== tags-input perf — small (3 chips, demo scale) ==")
    fmt("shuffle", small_shuffle_times)
    fmt("add-one", small_add_times)
    print("== tags-input perf — stress (500 chips) ==")
    fmt("shuffle", shuffle_times)
    fmt("add-one", add_times)

    # Animation diagnostic — sample the most-recently-added
    # chip's bounding rect + transform across the first 8 frames
    # so we can see if it's flying in from document origin or
    # animating in place.
    print("\n== add-one animation trace (newest chip across frames) ==")
    with serve(HARNESS_DIR) as base_url2, sync_playwright() as p2:
        browser = p2.chromium.launch(headless=True)
        page = browser.new_context().new_page()
        page.on(
            "console",
            lambda msg: print(f"  console[{msg.type}] {msg.text}")
            if any(k in msg.text for k in ["flip_with_new_rect", "flip_target"])
            else None,
        )
        page.goto(base_url2)
        page.wait_for_selector(".chip", timeout=10_000)

        # Re-enable transitions if the harness mount disabled them.
        # (App boots normally so should be fine, but be explicit.)
        page.evaluate("document.title = 'pre-add';")
        # Click add and immediately snapshot the new chip across
        # frames.
        page.locator("#add-one").click()
        # Sample 8 consecutive frames.
        samples = page.evaluate(
            """
            async () => {
              const out = [];
              for (let i = 0; i < 8; i++) {
                const last = document.querySelectorAll('.chip');
                const el = last[last.length - 1];
                if (!el) {
                  out.push({frame: i, missing: true});
                } else {
                  const r = el.getBoundingClientRect();
                  const cs = getComputedStyle(el);
                  out.push({
                    frame: i,
                    left: Math.round(r.left),
                    top: Math.round(r.top),
                    width: Math.round(r.width),
                    transform: cs.transform,
                  });
                }
                await new Promise(res => requestAnimationFrame(res));
              }
              return out;
            }
            """
        )
        for s in samples:
            print(f"  {s}")
        browser.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())

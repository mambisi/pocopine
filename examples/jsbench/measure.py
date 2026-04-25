#!/usr/bin/env python3
from __future__ import annotations

import contextlib
import http.server
import socketserver
import statistics
import threading
import time
from pathlib import Path

from playwright.sync_api import sync_playwright


ROOT = Path(__file__).resolve().parent
WARMUPS = 2
RUNS = 5


class Quiet(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *_args, **_kwargs):
        pass


class Reuse(socketserver.TCPServer):
    allow_reuse_address = True


@contextlib.contextmanager
def serve():
    handler = lambda *a, **kw: Quiet(*a, directory=str(ROOT), **kw)
    server = Reuse(("127.0.0.1", 0), handler)
    port = server.server_address[1]
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield f"http://127.0.0.1:{port}/"
    finally:
        server.shutdown()
        server.server_close()


def text(page, selector: str) -> str:
    return page.locator(selector).first.text_content(timeout=10_000).strip()


def row_count(page) -> int:
    return page.locator("tbody tr").count()


def measure_action(page, button_id: str, settle_ms: int) -> dict[str, object]:
    t0 = time.perf_counter()
    page.locator(f"#{button_id}").click()
    time.sleep(settle_ms / 1000.0)
    t1 = time.perf_counter()
    return {
        "button": button_id,
        "wall_ms": (t1 - t0) * 1000.0,
        "action": text(page, ".metrics span:nth-child(1) strong"),
        "app_ms": text(page, ".metrics span:nth-child(2) strong"),
        "rows": row_count(page),
    }


def main() -> int:
    with serve() as base_url, sync_playwright() as p:
        browser = p.firefox.launch(headless=True)
        page = browser.new_page(viewport={"width": 1280, "height": 1200})
        page.on("pageerror", lambda err: print(f"[PAGE ERROR] {err}"))
        page.on(
            "console",
            lambda msg: print(f"[CONSOLE {msg.type}] {msg.text}")
            if msg.type in ("error", "warning")
            else None,
        )
        page.goto(base_url)
        page.wait_for_selector("js-bench-app button#run", timeout=15_000)

        plan = [
            ("run", 100),
            ("clear", 50),
            ("run", 100),
            ("update", 100),
            ("swaprows", 100),
            ("clear", 50),
            ("runlots", 250),
            ("update", 150),
            ("add", 150),
            ("clear", 80),
        ]

        grouped: dict[str, list[float]] = {}
        for _ in range(WARMUPS):
            for button, settle in plan:
                measure_action(page, button, settle)

        for _ in range(RUNS):
            for button, settle in plan:
                result = measure_action(page, button, settle)
                grouped.setdefault(result["action"], []).append(result["wall_ms"])

        browser.close()

    for action, values in grouped.items():
        mean = statistics.mean(values)
        worst = max(values)
        print(f"{action:20} mean={mean:8.2f} ms max={worst:8.2f} ms n={len(values)}")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())

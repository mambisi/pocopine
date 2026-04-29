#!/usr/bin/env python3
"""jsbench harness driver.

Each subdirectory of `jsbench/` (pocopine, leptos, yew, vue, …)
is a self-contained harness — same buttons, same wired-up table,
implemented on a different framework. `measure.py` serves one
harness directory over a temporary HTTP server and drives it via
Playwright.

Modes:

  python measure.py [<harness-dir>]
      Run the bench plan (2 warmups + 5 measured passes) against
      the given harness directory (default: jsbench/pocopine).
      Reports mean / p50 / p95 / max wall-clock per action,
      plus a geometric mean across action means.

  python measure.py --profile-mount [<harness-dir>]
      Click `runLots(10000)` once with the pocopine
      mount-profiler enabled; capture and print the JSON line
      the profiler emits.

  python measure.py --profile-bench [<harness-dir>]
      Run the standard bench plan with profiling enabled. For
      each action, group the per-run profile JSON; print the
      slowest run's phase breakdown alongside the wall-clock
      distribution. Use this to diagnose actions whose mean is
      cheap but max is expensive — e.g. `clear` with a long
      tail.

Profiler note: the runtime emits one
`POCOPINE_MOUNT_PROFILE { … }` line per action when
`window.__POCOPINE_MOUNT_PROFILE = true` and pocopine-core was
built with `--features mount-profiler`. The `--profile-*` modes
are only meaningful against the pocopine harness.
"""
from __future__ import annotations

import argparse
import contextlib
import http.server
import json
import math
import socketserver
import statistics
import threading
import time
from pathlib import Path

from playwright.sync_api import sync_playwright


DEFAULT_ROOT = Path(__file__).resolve().parent / "pocopine"
WARMUPS = 2
RUNS = 5
PROFILE_PREFIX = "POCOPINE_MOUNT_PROFILE "

# Profile fields ordered for the diagnostic readout. Phase totals
# come first so the dominant cost surfaces immediately, then the
# sub-phase breakdowns. `unaccounted_ms` calls out residuals not
# yet instrumented.
PROFILE_PHASE_FIELDS = (
    ("measured_mount_ms", "mount"),
    ("reconcile_total_ms", "reconcile"),
    ("state_sync_total_ms", "state_sync"),
    ("unmount_total_ms", "unmount"),
    ("unaccounted_ms", "unaccounted"),
)
PROFILE_DETAIL_FIELDS = (
    ("clone_template_body_ms", "  mount.clone_template_body"),
    ("node_path_resolution_ms", "  mount.node_path_resolution"),
    ("initial_binding_apply_ms", "  mount.initial_binding_apply"),
    ("listener_installation_ms", "  mount.listener_installation"),
    ("dom_insertion_ms", "  mount.dom_insertion"),
    ("reconcile_pool_build_ms", "  reconcile.pool_build"),
    ("reconcile_row_iter_ms", "  reconcile.row_iter"),
    ("reconcile_leaver_drain_ms", "  reconcile.leaver_drain"),
    ("reconcile_reorder_ms", "  reconcile.reorder"),
    ("state_sync_closure_ms", "  state_sync.closure"),
    ("state_sync_invalidate_ms", "  state_sync.invalidate"),
    ("state_sync_trigger_ms", "  state_sync.trigger"),
)
PROFILE_COUNTER_FIELDS = (
    ("compiled_rows_mounted", "compiled_rows_mounted"),
    ("generic_rows_mounted", "generic_rows_mounted"),
    ("reconcile_runs", "reconcile_runs"),
    ("state_sync_runs", "state_sync_runs"),
    ("unmount_subtrees", "unmount_subtrees"),
)


class Quiet(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *_args, **_kwargs):
        pass


class Reuse(socketserver.TCPServer):
    allow_reuse_address = True


@contextlib.contextmanager
def serve(root: Path):
    handler = lambda *a, **kw: Quiet(*a, directory=str(root), **kw)
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


def click_action(page, action_id: str) -> None:
    if action_id == "select":
        page.locator("tbody tr:nth-child(2) td.col-md-4 a").click()
    elif action_id == "remove":
        page.locator("tbody tr:nth-child(2) td.col-md-1 a").click()
    else:
        page.locator(f"#{action_id}").click()


def measure_action(page, action_id: str, settle_ms: int) -> dict[str, object]:
    t0 = time.perf_counter()
    click_action(page, action_id)
    time.sleep(settle_ms / 1000.0)
    t1 = time.perf_counter()
    return {
        "button": action_id,
        "wall_ms": (t1 - t0) * 1000.0,
        "action": text(page, ".metrics span:nth-child(1) strong"),
        "app_ms": text(page, ".metrics span:nth-child(2) strong"),
        "rows": row_count(page),
    }


def fmt_ms(value: float) -> str:
    return f"{value:8.2f}"


def geometric_mean(values: list[float]) -> float:
    positives = [value for value in values if value > 0.0]
    if not positives:
        return 0.0
    return math.exp(sum(math.log(value) for value in positives) / len(positives))


def print_geomean(means: list[float]) -> None:
    if not means:
        return
    print("-" * 78)
    print(
        f"{'geomean':<22} {fmt_ms(geometric_mean(means))} "
        f"{'':>8} {'':>8} {'':>8} {'':>8} {len(means):>4}"
    )


def percentile(values: list[float], p: float) -> float:
    if not values:
        return 0.0
    if len(values) == 1:
        return values[0]
    sorted_values = sorted(values)
    rank = (p / 100.0) * (len(sorted_values) - 1)
    low = int(rank)
    high = min(low + 1, len(sorted_values) - 1)
    frac = rank - low
    return sorted_values[low] + (sorted_values[high] - sorted_values[low]) * frac


def standard_plan() -> list[tuple[str, int]]:
    # Close to the js-framework-benchmark keyed table action family:
    # create 1k, replace all, partial update, select row, swap rows,
    # remove row, clear, create many, clear, create 1k, append, clear.
    return [
        ("run", 100),
        ("run", 100),
        ("update", 100),
        ("select", 50),
        ("swaprows", 100),
        ("remove", 100),
        ("clear", 50),
        ("runlots", 250),
        ("clear", 80),
        ("run", 100),
        ("update", 100),
        ("add", 150),
        ("clear", 80),
    ]


def profile_plan() -> list[tuple[str, int]]:
    # Pocopine-only diagnostic plan. It keeps the standard
    # create/update/swap/remove/clear/append shape, then adds
    # structural prepend and full-reorder actions for RFC 064
    # Phase 4 profiling. The normal cross-framework bench plan
    # intentionally stays unchanged.
    return [
        ("run", 100),
        ("update", 100),
        ("swaprows", 100),
        ("remove", 100),
        ("clear", 80),
        ("run", 100),
        ("add", 150),
        ("prepend", 150),
        ("reorder", 150),
        ("clear", 80),
        ("runlots", 250),
        ("clear", 80),
    ]


def launch_browser(p, name: str):
    """Pick a Playwright browser by name. Defaults to firefox to
    preserve the historical bench numbers; --browser chromium runs
    against headless Chrome (Chromium / V8) for cross-engine
    comparisons."""
    if name == "chromium":
        return p.chromium.launch(headless=True)
    if name == "firefox":
        return p.firefox.launch(headless=True)
    if name == "webkit":
        return p.webkit.launch(headless=True)
    raise ValueError(f"unknown browser: {name}")


def profile_runlots(root: Path, browser_name: str) -> int:
    """Single-action runLots profiling. Same as before."""
    with serve(root) as base_url, sync_playwright() as p:
        browser = launch_browser(p, browser_name)
        page = browser.new_page(viewport={"width": 1280, "height": 1200})
        reports: list[str] = []

        page.add_init_script("window.__POCOPINE_MOUNT_PROFILE = true;")
        page.on("pageerror", lambda err: print(f"[PAGE ERROR] {err}"))
        page.on(
            "console",
            lambda msg: (
                reports.append(msg.text[len(PROFILE_PREFIX):])
                if msg.text.startswith(PROFILE_PREFIX)
                else print(f"[CONSOLE {msg.type}] {msg.text}")
                if msg.type in ("error", "warning")
                else None
            ),
        )
        page.goto(base_url)
        page.wait_for_selector("#runlots", timeout=15_000)
        page.locator("#runlots").click()
        page.wait_for_function(
            "document.querySelectorAll('tbody tr').length === 10000", timeout=30_000
        )
        page.wait_for_timeout(100)
        browser.close()

    if not reports:
        print("no mount profile report captured")
        return 1
    print(f"{PROFILE_PREFIX}{reports[-1]}")
    return 0


def profile_bench(root: Path, browser_name: str) -> int:
    """Run the full bench plan with profiling enabled. Capture
    per-run profile JSON, group by action, report wall-clock
    distribution + slowest-run phase breakdown for each action.
    Best for diagnosing high-variance actions like `clear`."""
    with serve(root) as base_url, sync_playwright() as p:
        browser = launch_browser(p, browser_name)
        page = browser.new_page(viewport={"width": 1280, "height": 1200})
        page.add_init_script("window.__POCOPINE_MOUNT_PROFILE = true;")

        # Profile reports flow in via console.log; pair them with
        # the most recent click so each action gets its own JSON.
        captured: list[dict] = []
        page.on("pageerror", lambda err: print(f"[PAGE ERROR] {err}"))
        page.on(
            "console",
            lambda msg: (
                _append_profile(captured, msg.text)
                if msg.text.startswith(PROFILE_PREFIX)
                else print(f"[CONSOLE {msg.type}] {msg.text}")
                if msg.type in ("error", "warning")
                else None
            ),
        )
        page.goto(base_url)
        page.wait_for_selector("#run", timeout=15_000)

        plan = profile_plan()
        # Walls per action; profiles per action (parallel ordering).
        walls: dict[str, list[float]] = {}
        profiles: dict[str, list[dict]] = {}

        for _ in range(WARMUPS):
            for button, settle in plan:
                measure_action(page, button, settle)
        captured.clear()  # Drop warmup profiles.

        for _ in range(RUNS):
            for button, settle in plan:
                # Snapshot how many profiles we already have so we
                # can match the new one(s) generated by THIS click.
                pre = len(captured)
                result = measure_action(page, button, settle)
                action = result["action"]
                walls.setdefault(action, []).append(result["wall_ms"])
                # Pair the per-run wall with whatever profile
                # report(s) the action emitted (almost always one).
                profile = captured[pre:] if len(captured) > pre else []
                if profile:
                    # If the action emitted multiple, keep the last
                    # — that's the report for the action under
                    # measurement (the deferred Closure::once fires
                    # AFTER the bench harness's measure() returns).
                    profiles.setdefault(action, []).append(profile[-1])

        browser.close()

    print_bench_distribution(walls, profiles)
    return 0


def _append_profile(captured: list[dict], text: str) -> None:
    payload = text[len(PROFILE_PREFIX):]
    try:
        captured.append(json.loads(payload))
    except json.JSONDecodeError:
        # Non-JSON line under the prefix shouldn't happen, but be
        # tolerant — log and drop.
        print(f"[BAD PROFILE] {payload}")


def print_bench_distribution(
    walls: dict[str, list[float]], profiles: dict[str, list[dict]]
) -> None:
    print()
    print(
        f"{'action':<22} {'mean':>8} {'p50':>8} {'p95':>8} {'max':>8} "
        f"{'spread':>8}    n"
    )
    print("-" * 78)
    means = []
    for action, values in walls.items():
        if not values:
            continue
        mean = statistics.mean(values)
        means.append(mean)
        p50 = percentile(values, 50)
        p95 = percentile(values, 95)
        worst = max(values)
        spread = (worst / mean) if mean > 0 else 0.0
        print(
            f"{action:<22} {fmt_ms(mean)} {fmt_ms(p50)} {fmt_ms(p95)} "
            f"{fmt_ms(worst)} {spread:7.2f}x {len(values):>4}"
        )
    print_geomean(means)

    print()
    print("Slowest-run phase breakdown:")
    print(
        "(action_total_ms is what the action paid; the phase rows below add up "
        "to it modulo unaccounted residual)"
    )
    print()
    for action, values in walls.items():
        if not values or action not in profiles or not profiles[action]:
            continue
        # The slowest run's profile points at where variance lives.
        slowest_idx = max(range(len(values)), key=lambda i: values[i])
        if slowest_idx >= len(profiles[action]):
            continue
        prof = profiles[action][slowest_idx]
        wall = values[slowest_idx]
        action_total = prof.get("action_total_ms", 0.0)
        print(
            f"{action:<22} slowest run wall_ms={wall:7.2f} "
            f"action_total_ms={action_total:7.2f}"
        )
        for key, label in PROFILE_PHASE_FIELDS:
            value = prof.get(key, 0.0)
            if value <= 0.01 and key != "unaccounted_ms":
                continue
            pct = (value / action_total * 100.0) if action_total > 0 else 0.0
            print(f"  {label:<22} {value:8.2f}  ({pct:5.1f}%)")
        # Sub-phase detail only when the parent phase is non-trivial.
        for key, label in PROFILE_DETAIL_FIELDS:
            value = prof.get(key, 0.0)
            if value <= 0.5:
                continue
            print(f"  {label:<22} {value:8.2f}")
        # Counters (no time, just identity check).
        counter_line = ", ".join(
            f"{name}={int(prof.get(key, 0))}" for key, name in PROFILE_COUNTER_FIELDS
        )
        print(f"  counters             : {counter_line}")
        print()


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("root", nargs="?", default=str(DEFAULT_ROOT))
    parser.add_argument(
        "--profile-mount",
        action="store_true",
        help="single runLots profile (legacy mode)",
    )
    parser.add_argument(
        "--profile-bench",
        action="store_true",
        help="full bench plan with per-run profile + distribution",
    )
    parser.add_argument(
        "--browser",
        choices=("firefox", "chromium", "webkit"),
        default="firefox",
        help="Playwright browser engine. Defaults to firefox so "
        "numbers stay comparable across the existing ledger.",
    )
    args = parser.parse_args()

    root = Path(args.root).resolve()
    if args.profile_mount:
        return profile_runlots(root, args.browser)
    if args.profile_bench:
        return profile_bench(root, args.browser)

    with serve(root) as base_url, sync_playwright() as p:
        browser = launch_browser(p, args.browser)
        page = browser.new_page(viewport={"width": 1280, "height": 1200})
        page.on("pageerror", lambda err: print(f"[PAGE ERROR] {err}"))
        page.on(
            "console",
            lambda msg: print(f"[CONSOLE {msg.type}] {msg.text}")
            if msg.type in ("error", "warning")
            else None,
        )
        page.goto(base_url)
        page.wait_for_selector("#run", timeout=15_000)

        plan = standard_plan()
        grouped: dict[str, list[float]] = {}
        for _ in range(WARMUPS):
            for button, settle in plan:
                measure_action(page, button, settle)

        for _ in range(RUNS):
            for button, settle in plan:
                result = measure_action(page, button, settle)
                grouped.setdefault(result["action"], []).append(result["wall_ms"])

        browser.close()

    print(
        f"{'action':<22} {'mean':>8} {'p50':>8} {'p95':>8} {'max':>8} "
        f"{'spread':>8}    n"
    )
    print("-" * 78)
    means = []
    for action, values in grouped.items():
        if not values:
            continue
        mean = statistics.mean(values)
        means.append(mean)
        p50 = percentile(values, 50)
        p95 = percentile(values, 95)
        worst = max(values)
        spread = (worst / mean) if mean > 0 else 0.0
        print(
            f"{action:<22} {fmt_ms(mean)} {fmt_ms(p50)} {fmt_ms(p95)} "
            f"{fmt_ms(worst)} {spread:7.2f}x {len(values):>4}"
        )
    print_geomean(means)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

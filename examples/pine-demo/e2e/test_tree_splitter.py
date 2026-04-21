#!/usr/bin/env python3
"""End-to-end checks for Pine's Tree + Splitter demos.

Boots a throwaway HTTP server rooted at `examples/pine-demo/`,
opens the demo in Firefox via Playwright, and exercises the two
primitives the user reported as broken in DOM dumps:

  - Tree: aria-level per depth, aria-hidden on collapsed subtrees,
          chevron click expands + reveals children, leaf chevrons
          stay display:none, click selects.
  - Splitter: initial flex-basis percentages match Panel defaults,
              each handle's aria-valuenow tracks its BEFORE-panel's
              size (not Panel 0 for every handle), keyboard
              step_inc on handle 2 shifts 1% between Panels 1 and 2.

Run:
    # one-time setup (global, user-level)
    python3 -m pip install --user --break-system-packages playwright
    python3 -m playwright install firefox
    # build the demo wasm
    (cd examples/pine-demo && wasm-pack build --target web --out-dir pkg --dev)
    # run
    python3 examples/pine-demo/e2e/test_tree_splitter.py
"""

from __future__ import annotations

import contextlib
import http.server
import socketserver
import sys
import threading
from pathlib import Path

from playwright.sync_api import sync_playwright, expect


DEMO_ROOT = Path(__file__).resolve().parent.parent


class _QuietHandler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *_args, **_kwargs):  # silence request logs
        pass


class _ReusableTCPServer(socketserver.TCPServer):
    allow_reuse_address = True


@contextlib.contextmanager
def _serve():
    """Serve `examples/pine-demo/` on a kernel-assigned free port.
    Yields the base URL."""
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


def _fail(msg: str) -> None:
    print(f"  ✗ {msg}", file=sys.stderr)
    raise AssertionError(msg)


def _ok(msg: str) -> None:
    print(f"  ✓ {msg}")


def run_tree_checks(page) -> None:
    print("Tree")

    # Top-level item: aria-level=1, not hidden.
    item_src = page.locator(
        'pine-tree-item[value="src"] > div[role="treeitem"]'
    ).first
    expect(item_src).to_have_attribute("aria-level", "1")
    expect(item_src).not_to_have_attribute("aria-hidden", "true")
    _ok("top-level src: aria-level=1, not hidden")

    # Nested leaf: aria-level=2, hidden (parent collapsed).
    item_lib = page.locator(
        'pine-tree-item[value="src/lib.rs"] > div[role="treeitem"]'
    )
    expect(item_lib).to_have_attribute("aria-level", "2")
    expect(item_lib).to_have_attribute("aria-hidden", "true")
    _ok("nested src/lib.rs: aria-level=2, hidden while parent collapsed")

    # Triple-nested: aria-level=3.
    item_btn = page.locator(
        'pine-tree-item[value="src/components/Button.rs"] > div[role="treeitem"]'
    )
    expect(item_btn).to_have_attribute("aria-level", "3")
    _ok("triple-nested src/components/Button.rs: aria-level=3")

    # Leaf items authored without <pine-tree-item-toggle> simply
    # don't render one — the demo puts a spacer <span> there instead.
    leaf_toggle = page.locator(
        'pine-tree-item[value="Cargo.toml"] > div > pine-tree-item-toggle'
    )
    if leaf_toggle.count() > 0:
        _fail("Cargo.toml has a toggle — template only renders it when authored")
    _ok("Cargo.toml (leaf) has no chevron")

    # Click the src chevron. The Toggle custom tag lives inside the
    # Item's rendered inner <div>, not as a direct child of the
    # <pine-tree-item> custom tag.
    src_toggle = page.locator(
        'pine-tree-item[value="src"] > div > pine-tree-item-toggle > button'
    )
    expect(src_toggle).to_be_visible()
    src_toggle.click()

    # lib.rs should become visible, src's aria-expanded = true.
    expect(item_lib).not_to_have_attribute("aria-hidden", "true")
    expect(item_src).to_have_attribute("aria-expanded", "true")
    expect(item_src).to_have_attribute("data-state", "open")
    _ok("click chevron → src expands, lib.rs visible, aria-expanded=true")

    # Click lib.rs — selection flips.
    item_lib.click()
    expect(item_lib).to_have_attribute("aria-selected", "true")
    _ok("click leaf → aria-selected=true")


def run_splitter_checks(page) -> None:
    print("Splitter")

    panels = page.locator(".pine-splitter-panel")
    sidebar = panels.nth(0)
    main = panels.nth(1)
    details = panels.nth(2)

    # Defaults are 25 / 50 / 25 (committed demo; user's local may
    # differ — assert the PARSED sum matches whatever's there).
    def _basis(el) -> float:
        style = el.get_attribute("style") or ""
        # style: "flex: 0 0 25%; overflow: hidden;"
        for frag in style.split(";"):
            frag = frag.strip()
            if frag.startswith("flex:"):
                pieces = frag.split()
                return float(pieces[3].rstrip("%"))
        raise AssertionError(f"no flex in style: {style!r}")

    a, b, c = _basis(sidebar), _basis(main), _basis(details)
    if abs(a + b + c - 100.0) > 0.01:
        _fail(f"panel sizes don't sum to 100: {a} + {b} + {c} = {a + b + c}")
    _ok(f"initial sizes sum to 100 ({a} / {b} / {c})")

    # Handles: handle 1 tracks sizes[0], handle 2 tracks sizes[1].
    handles = page.locator(".pine-splitter-resize-handle")
    h1 = handles.nth(0)
    h2 = handles.nth(1)
    h1_val = float(h1.get_attribute("aria-valuenow"))
    h2_val = float(h2.get_attribute("aria-valuenow"))
    if abs(h1_val - a) > 0.01:
        _fail(f"handle 1 aria-valuenow={h1_val}, want {a}")
    if abs(h2_val - b) > 0.01:
        _fail(f"handle 2 aria-valuenow={h2_val}, want {b} (NOT {a} — "
              "regression guard for the neighbour_indices bug)")
    _ok("each handle's aria-valuenow tracks its own before-panel, not Panel 0")

    # Keyboard: focus handle 2, ArrowRight — Panel 1 grows by 1%,
    # Panel 2 shrinks by 1%, Panel 0 is untouched.
    h2.focus()
    page.keyboard.press("ArrowRight")
    a2, b2, c2 = _basis(sidebar), _basis(main), _basis(details)
    if abs(a - a2) > 0.01:
        _fail(f"Panel 0 should not change on handle-2 keyboard: {a} -> {a2}")
    if abs((b + 1.0) - b2) > 0.01:
        _fail(f"Panel 1 should grow by 1%: {b} -> {b2} (want {b + 1.0})")
    if abs((c - 1.0) - c2) > 0.01:
        _fail(f"Panel 2 should shrink by 1%: {c} -> {c2} (want {c - 1.0})")
    _ok("ArrowRight on handle 2 shifts 1% from Panel 2 into Panel 1")


def main() -> int:
    pkg = DEMO_ROOT / "pkg" / "pine_demo_bg.wasm"
    if not pkg.exists():
        print(f"error: {pkg} missing — run:", file=sys.stderr)
        print(
            "  (cd examples/pine-demo && wasm-pack build --target web --out-dir pkg --dev)",
            file=sys.stderr,
        )
        return 2

    with _serve() as url, sync_playwright() as pw:
        browser = pw.firefox.launch(headless=True)
        context = browser.new_context()
        page = context.new_page()
        page.goto(url)
        page.wait_for_load_state("networkidle")
        # Give the wasm runtime a beat to mount + run on_mount +
        # after_flush-scheduled has_children checks.
        page.wait_for_timeout(250)

        try:
            run_tree_checks(page)
            run_splitter_checks(page)
        finally:
            browser.close()

    print("\nAll Pine demo e2e checks passed.")
    return 0


if __name__ == "__main__":
    sys.exit(main())

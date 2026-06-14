/* pocopine boot loader — injected into pkg/index.html by `pocopine build`.
 *
 * Runs as a classic <script> BEFORE the app's module boot, so it wraps
 * `fetch` to report the real wasm download as progress. Installs the
 * `window.__pocopineProgress` controller that `pocopine::progress` (and
 * the router auto-hook) reuse in-app. The full-screen logo splash is
 * removed — and the bar finished — when core reports the app ready
 * (`progress.ready()` from AppBootCompleted) or, for router apps, when
 * the first route paint completes.
 *
 * Keep the controller contract in sync with the fallback in
 * crates/pocopine-core/src/progress.rs. */
(function () {
  if (window.__pocopineProgress) return;

  var el = null, bar = null, pending = 0, value = 0, determinate = false, trickle = 0, resume = 0, showTimer = 0, shown = false;
  // A navigation may run this long before the bar reveals (deferred
  // indicator): fast routes never flash a bar; only genuinely slow work does.
  var SHOW_DELAY = 150;

  function ensure() {
    el = document.getElementById("pp-loader");
    if (!el) {
      el = document.createElement("div");
      el.id = "pp-loader";
      el.setAttribute("role", "progressbar");
      el.setAttribute("aria-label", "Loading");
      el.setAttribute("aria-valuemin", "0");
      el.setAttribute("aria-valuemax", "100");
      bar = document.createElement("div");
      bar.className = "pp-loader__bar";
      el.appendChild(bar);
      (document.body || document.documentElement).appendChild(el);
    }
    if (!bar) bar = el.querySelector(".pp-loader__bar");
  }
  function render() {
    ensure();
    var v = Math.max(0, Math.min(value, 1));
    if (bar) bar.style.transform = "translateX(" + (v - 1) * 100 + "%)";
    el.setAttribute("aria-valuenow", String(Math.round(v * 100)));
  }
  function show() { ensure(); el.classList.remove("pp-loader--done", "pp-loader--error"); }
  // Reveal the bar (the deferred timer fired, or an explicit show): start the
  // indeterminate trickle from a small head start.
  function reveal() {
    showTimer = 0; shown = true; show();
    if (value < 0.08) value = 0.08;
    render(); startTrickle();
  }
  function startTrickle() {
    clearInterval(trickle);
    trickle = setInterval(function () {
      if (value < 0.95) { value += (0.95 - value) * 0.1; render(); }
    }, 200);
  }
  function hideSplash() {
    var s = document.getElementById("pp-splash");
    if (!s) return;
    s.classList.add("pp-splash--done");
    s.addEventListener("transitionend", function () { s.remove(); }, { once: true });
    setTimeout(function () { s.remove(); }, 700);
  }
  function complete() {
    clearTimeout(showTimer); showTimer = 0;
    clearInterval(trickle); clearTimeout(resume);
    pending = 0; value = 1; render();
    ensure(); el.classList.add("pp-loader--done");
    hideSplash();
    setTimeout(function () {
      value = 0; determinate = false; shown = false;
      if (bar) {
        bar.style.transition = "none"; render();
        requestAnimationFrame(function () { if (bar) bar.style.transition = ""; });
      }
      if (el) el.classList.remove("pp-loader--done");
    }, 450);
  }

  var api = {
    // Begin a task. Don't show yet — arm a timer so a navigation that finishes
    // within SHOW_DELAY never flashes the bar. Slow work reveals it on fire.
    start: function () {
      pending++;
      if (pending === 1 && !determinate && !shown && !showTimer) {
        showTimer = setTimeout(reveal, SHOW_DELAY);
      }
    },
    set: function (f) {
      // A concrete fraction is an explicit "show progress" — reveal at once.
      clearTimeout(showTimer); showTimer = 0; shown = true;
      determinate = true; clearInterval(trickle); clearTimeout(resume);
      value = Math.max(0, Math.min(f, 1)); show(); render();
      if (value >= 1) { complete(); return; }
      resume = setTimeout(function () { determinate = false; startTrickle(); }, 400);
    },
    inc: function (d) { api.set(value + d); },
    done: function () {
      pending = Math.max(0, pending - 1);
      if (pending > 0) return;
      // Finished before the reveal timer fired → cancel it; never flash.
      if (showTimer) { clearTimeout(showTimer); showTimer = 0; return; }
      if (shown) complete();
    },
    error: function () {
      clearTimeout(showTimer); showTimer = 0; shown = true;
      clearInterval(trickle); clearTimeout(resume); ensure();
      el.classList.add("pp-loader--error"); value = 1; render(); hideSplash();
    },
    // App reported ready (AppBootCompleted / first route paint): drop the
    // splash and finish the boot bar regardless of in-flight count.
    ready: function () { complete(); },
  };
  window.__pocopineProgress = api;

  // Track the wasm download as determinate progress. wasm-bindgen's init()
  // fetches the (hashed) *_bg.wasm via the global fetch; wrap that one
  // response body in a byte counter, mapping onto the first 90% of the bar
  // (the tail trickles through compile/mount). The re-wrapped Response keeps
  // its headers so streaming compilation still works. Restored on ready.
  var realFetch = window.fetch.bind(window);
  window.fetch = function (input, opts) {
    var url = typeof input === "string" ? input : (input && input.url) || String(input);
    var res = realFetch(input, opts);
    if (!/\.wasm(\?|$)/.test(url)) return res;
    return res.then(function (resp) {
      if (!resp.body || !resp.ok) return resp;
      var total = Number(resp.headers.get("content-length")) || 0;
      var loaded = 0;
      var reader = resp.body.getReader();
      var stream = new ReadableStream({
        pull: function (controller) {
          return reader.read().then(function (r) {
            if (r.done) { window.fetch = realFetch; controller.close(); return; }
            loaded += r.value.byteLength;
            if (total) api.set((loaded / total) * 0.9);
            controller.enqueue(r.value);
          });
        },
        cancel: function (reason) { reader.cancel(reason); },
      });
      return new Response(stream, { headers: resp.headers, status: resp.status, statusText: resp.statusText });
    });
  };
})();

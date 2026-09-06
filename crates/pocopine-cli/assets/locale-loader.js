/* Starts the selected catalog before deferred wasm boot. The wasm validates
 * this hint and all downloaded bytes before marking the catalog ready. */
(function () {
  var island = document.getElementById("pp-locale-manifest");
  if (!island || window.__pocopineLocale) return;
  var data = JSON.parse(island.textContent), manifest = data.manifest;
  var supported = Object.create(null), parents = Object.create(null);
  Object.keys(manifest.catalogs).forEach(function (tag) { supported[tag.toLowerCase()] = tag; });
  Object.keys(data.fallbacks).forEach(function (tag) { parents[tag.toLowerCase()] = data.fallbacks[tag]; });

  function match(value) {
    if (typeof value !== "string" || value.length > 128) return null;
    var parts = value.toLowerCase().split("-"), i = 1, seen = Object.create(null);
    if (!/^[a-z]{2,8}$/.test(parts[0])) return null;
    if (/^[a-z]{4}$/.test(parts[i] || "")) i++;
    if (/^(?:[a-z]{2}|[0-9]{3})$/.test(parts[i] || "")) i++;
    for (; i < parts.length; i++) {
      if (!/^(?:[a-z0-9]{5,8}|[0-9][a-z0-9]{3})$/.test(parts[i]) || seen[parts[i]]) return null;
      seen[parts[i]] = true;
    }
    while (parts.length) {
      var tag = parts.join("-");
      if (supported[tag]) return supported[tag];
      if (Object.prototype.hasOwnProperty.call(parents, tag)) return parents[tag];
      parts.pop();
    }
    return null;
  }
  var cookie = null, visited = false;
  try {
    document.cookie.split(";").some(function (entry) {
      var pair = entry.trim().split("=");
      if (pair[0] === "pocopine_locale_visited" && pair[1] === "1") visited = true;
      if (pair[0] !== "pocopine_locale") return false;
      cookie = pair[1]; return false;
    });
  } catch (_) {}
  try {
    cookie = cookie || sessionStorage.getItem("pocopine_locale");
    visited = visited || sessionStorage.getItem("pocopine_locale_visited") === "1";
  } catch (_) {}
  var segment = location.pathname.split("/")[1];
  var route = manifest.config.routing !== "none" && supported[segment.toLowerCase()] || null;
  var languages = Array.from(navigator.languages || [navigator.language]);
  // Only the first bare-root visit negotiates a redirect. Other unprefixed
  // pages always mean the default, regardless of cookies or browser language.
  var detect = manifest.config.routing === "none" || (!visited && location.pathname === "/");
  var selected = route || (!detect && manifest.config.default) || match(cookie);
  for (var i = 0; !selected && i < languages.length; i++) selected = match(languages[i]);
  selected = selected || manifest.config.default;
  var pending = new Map();
  var api = {
    manifest: manifest, selected: selected, route: route, cookie: cookie,
    accepted: languages.join(","), visited: visited,
    url: location.pathname + (location.search || "") + (location.hash || ""), ready: false, appReady: false,
    load: function (url) {
      if (!pending.has(url)) {
        var task = fetch(url, { credentials: "same-origin" }).then(function (response) {
          if (!response.ok) throw new Error("catalog HTTP " + response.status);
          return response.arrayBuffer();
        });
        pending.set(url, task);
        task.catch(function () { if (pending.get(url) === task) pending.delete(url); });
      }
      return pending.get(url);
    },
    markReady: function () {
      api.ready = true;
      var error = document.getElementById("pp-locale-error");
      if (error) error.remove();
      if (api.appReady && window.__pocopineProgress) window.__pocopineProgress.ready();
    },
    fail: function () {
      if (window.__pocopineProgress) window.__pocopineProgress.error();
      if (document.getElementById("pp-locale-error")) return;
      var error = document.createElement("div");
      error.id = "pp-locale-error"; error.setAttribute("role", "alert");
      error.style.cssText = "position:fixed;inset:0;display:grid;place-content:center;gap:1rem;background:Canvas;color:CanvasText;z-index:10000";
      var message = document.createElement("p"); message.textContent = "Unable to load this page.";
      var retry = document.createElement("button"); retry.textContent = "Reload";
      retry.addEventListener("click", function () { location.reload(); });
      error.append(message, retry); (document.body || document.documentElement).appendChild(error);
    }
  };
  window.__pocopineLocale = api;
  var url = manifest.catalogs[selected];
  var link = document.createElement("link");
  link.rel = "preload"; link.as = "fetch"; link.crossOrigin = "anonymous"; link.href = url;
  document.head.appendChild(link);
  api.load(url).catch(api.fail);
})();

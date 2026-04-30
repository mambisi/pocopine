let activeRoute = null;

const routes = [
  {
    test: (path) => path === "/",
    load: () => import("/pkg/hn_route_list.js"),
  },
  {
    test: (path) => /^\/item\/[^/]+$/.test(path),
    load: () => import("/pkg/hn_route_detail.js"),
  },
  {
    test: () => true,
    load: () => import("/pkg/hn_route_not_found.js"),
  },
];

function matchRoute(path) {
  return routes.find((route) => route.test(path)) || routes[routes.length - 1];
}

async function mountCurrentRoute() {
  const outlet = document.querySelector("pp-outlet");
  if (!outlet) return;

  if (activeRoute?.unmount_hn_route) {
    activeRoute.unmount_hn_route();
  }

  const mod = await matchRoute(location.pathname).load();
  await mod.default();
  mod.mount_hn_route(outlet, location.pathname);
  activeRoute = mod;
}

document.addEventListener("click", (event) => {
  const anchor = event.target.closest?.("a[pp-route]");
  if (!anchor) return;

  const url = new URL(anchor.getAttribute("href"), location.href);
  if (url.origin !== location.origin) return;

  event.preventDefault();
  if (url.pathname === location.pathname && url.search === location.search) {
    return;
  }
  history.pushState(null, "", url);
  mountCurrentRoute();
});

window.addEventListener("popstate", () => {
  mountCurrentRoute();
});

export async function startHnSplitApp() {
  const shell = await import("/pkg/hn.js");
  await shell.default();
  await mountCurrentRoute();
}

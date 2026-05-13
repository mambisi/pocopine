use std::collections::BTreeSet;

use axum::Router;

/// A server-function route registered by macro-generated host code.
///
/// Values are collected through `inventory`; applications normally never
/// construct this type directly.
#[derive(Clone, Copy)]
pub struct ServerFunctionRoute {
    pub module_path: &'static str,
    pub function: &'static str,
    pub path: fn() -> &'static str,
    pub install: fn(Router) -> Router,
}

/// Duplicate server-function route path detected while finalizing a server.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServerFunctionRouteConflict {
    pub path: &'static str,
    pub first: String,
    pub second: String,
}

impl ServerFunctionRouteConflict {
    fn new(path: &'static str, first: ServerFunctionRoute, second: ServerFunctionRoute) -> Self {
        Self {
            path,
            first: route_label(first),
            second: route_label(second),
        }
    }
}

inventory::collect!(ServerFunctionRoute);

/// Installs all linked `#[server]` functions into `router`.
///
/// Duplicate paths are reported instead of installed. The first route wins
/// only long enough for `Server::try_finalize` to return a structured
/// validation error; real servers should treat any conflict as a startup
/// failure.
pub fn install_server_functions(mut router: Router) -> (Router, Vec<ServerFunctionRouteConflict>) {
    let mut routes: Vec<_> = inventory::iter::<ServerFunctionRoute>
        .into_iter()
        .copied()
        .collect();
    routes.sort_by(|left, right| {
        let left_path = (left.path)();
        let right_path = (right.path)();
        (left_path, left.module_path, left.function).cmp(&(
            right_path,
            right.module_path,
            right.function,
        ))
    });

    let mut installed_paths = BTreeSet::new();
    let mut installed_routes = Vec::new();
    let mut conflicts = Vec::new();

    for route in routes {
        let path = (route.path)();
        if installed_paths.insert(path) {
            router = (route.install)(router);
            installed_routes.push(route);
            continue;
        }

        let first = installed_routes
            .iter()
            .copied()
            .find(|candidate| (candidate.path)() == path)
            .expect("installed route path should have a route entry");
        conflicts.push(ServerFunctionRouteConflict::new(path, first, route));
    }

    (router, conflicts)
}

fn route_label(route: ServerFunctionRoute) -> String {
    format!("{}::{}", route.module_path, route.function)
}

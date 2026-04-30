//! RFC 065 Phase 1 route-cluster runtime.
//!
//! Phase 1 is intentionally single-binary: every vtable is already
//! present in the compiled wasm. The loader therefore only computes
//! ownership, registers vtables from in-binary cluster maps, and
//! memoizes which clusters are installed.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::registry::ComponentVTable;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cluster {
    pub name: String,
    pub components: Vec<&'static str>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouteClusterPlan {
    pub route_id: &'static str,
    pub component: &'static str,
    pub clusters: Vec<String>,
}

#[derive(Clone, Copy)]
pub struct ClusterManifest {
    pub components: &'static [&'static ComponentVTable],
    pub routes: &'static [RouteClusterRoot],
}

#[derive(Clone, Copy)]
pub struct RouteClusterRoot {
    pub route_id: &'static str,
    pub component: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClusterLoadError {
    NoManifest,
    UnknownCluster(String),
    UnknownRouteComponent(&'static str),
    MissingComponent {
        owner: &'static str,
        missing: &'static str,
    },
}

#[derive(Default)]
struct InstalledManifest {
    clusters: BTreeMap<String, Vec<&'static ComponentVTable>>,
    route_plans: HashMap<&'static str, Vec<String>>,
    installed: BTreeSet<String>,
}

thread_local! {
    static ACTIVE_MANIFEST: RefCell<Option<InstalledManifest>> = const { RefCell::new(None) };
}

pub fn install_cluster_manifest(
    manifest: &'static ClusterManifest,
) -> Result<(), ClusterLoadError> {
    let planned = compute_clusters(manifest)?;
    ACTIVE_MANIFEST.with(|active| {
        *active.borrow_mut() = Some(planned);
    });
    install_shell()
}

pub fn install_shell() -> Result<(), ClusterLoadError> {
    ensure_clusters_ready(&["shell"])
}

pub fn ensure_clusters(names: &'static [&'static str]) -> Result<(), ClusterLoadError> {
    ensure_clusters_ready(names)
}

pub fn ensure_clusters_ready(names: &[&str]) -> Result<(), ClusterLoadError> {
    ACTIVE_MANIFEST.with(|active| {
        let mut active = active.borrow_mut();
        let Some(manifest) = active.as_mut() else {
            return Err(ClusterLoadError::NoManifest);
        };
        for name in names {
            if manifest.installed.contains(*name) {
                continue;
            }
            let Some(vtables) = manifest.clusters.get(*name) else {
                return Err(ClusterLoadError::UnknownCluster((*name).to_string()));
            };
            for vtable in vtables {
                if !vtable.is_bundle {
                    (vtable.register)();
                }
            }
            manifest.installed.insert((*name).to_string());
        }
        Ok(())
    })
}

pub fn prefetch_clusters(_names: &'static [&'static str]) {
    // Phase 1 has no fetch path. The hook is present so Phase 2 can
    // attach network prefetch without changing user code.
}

pub fn ensure_route_component_ready(component: &'static str) -> Result<(), ClusterLoadError> {
    let clusters = ACTIVE_MANIFEST.with(|active| {
        let active = active.borrow();
        let manifest = active.as_ref().ok_or(ClusterLoadError::NoManifest)?;
        manifest
            .route_plans
            .get(component)
            .cloned()
            .ok_or(ClusterLoadError::UnknownRouteComponent(component))
    })?;
    let names: Vec<&str> = clusters.iter().map(String::as_str).collect();
    ensure_clusters_ready(&names)
}

pub fn installed_clusters_for_test() -> Vec<String> {
    ACTIVE_MANIFEST.with(|active| {
        active
            .borrow()
            .as_ref()
            .map(|manifest| manifest.installed.iter().cloned().collect())
            .unwrap_or_default()
    })
}

pub fn cluster_plan_for_test(
    manifest: &'static ClusterManifest,
) -> Result<Vec<Cluster>, ClusterLoadError> {
    let planned = compute_clusters(manifest)?;
    Ok(planned
        .clusters
        .into_iter()
        .map(|(name, vtables)| Cluster {
            name,
            components: vtables.into_iter().map(|v| v.name).collect(),
        })
        .collect())
}

pub fn route_plans_for_test(
    manifest: &'static ClusterManifest,
) -> Result<Vec<RouteClusterPlan>, ClusterLoadError> {
    let planned = compute_clusters(manifest)?;
    Ok(manifest
        .routes
        .iter()
        .map(|route| RouteClusterPlan {
            route_id: route.route_id,
            component: route.component,
            clusters: planned
                .route_plans
                .get(route.component)
                .cloned()
                .unwrap_or_default(),
        })
        .collect())
}

fn compute_clusters(
    manifest: &'static ClusterManifest,
) -> Result<InstalledManifest, ClusterLoadError> {
    let by_name: HashMap<&'static str, &'static ComponentVTable> =
        manifest.components.iter().map(|v| (v.name, *v)).collect();

    for component in manifest.components {
        for used in (component.uses)() {
            if !by_name.contains_key(used.name) {
                return Err(ClusterLoadError::MissingComponent {
                    owner: component.name,
                    missing: used.name,
                });
            }
        }
    }

    let mut route_closures: Vec<(&RouteClusterRoot, BTreeSet<&'static str>)> = Vec::new();
    for route in manifest.routes {
        let Some(root) = by_name.get(route.component).copied() else {
            return Err(ClusterLoadError::UnknownRouteComponent(route.component));
        };
        let mut closure = BTreeSet::new();
        collect_closure(root, &by_name, &mut closure)?;
        route_closures.push((route, closure));
    }

    let route_count = route_closures.len();
    let mut component_routes: BTreeMap<&'static str, BTreeSet<&'static str>> = BTreeMap::new();
    for (route, closure) in &route_closures {
        for component in closure {
            component_routes
                .entry(*component)
                .or_default()
                .insert(route.route_id);
        }
    }

    let mut clusters_by_name: BTreeMap<String, BTreeSet<&'static str>> = BTreeMap::new();
    for (component, routes) in component_routes {
        let cluster_name = if route_count == 0 || routes.len() == route_count {
            "shell".to_string()
        } else if routes.len() == 1 {
            format!("route:{}", routes.iter().next().expect("one route"))
        } else {
            let signature = routes.iter().copied().collect::<Vec<_>>().join("+");
            format!("shared:{signature}")
        };
        clusters_by_name
            .entry(cluster_name)
            .or_default()
            .insert(component);
    }
    let assigned: BTreeSet<&'static str> = clusters_by_name
        .values()
        .flat_map(|members| members.iter().copied())
        .collect();
    let shell = clusters_by_name.entry("shell".to_string()).or_default();
    for component in manifest.components {
        if !assigned.contains(component.name) {
            shell.insert(component.name);
        }
    }

    let mut route_plans = HashMap::new();
    for (route, closure) in &route_closures {
        let mut names = BTreeSet::new();
        for (cluster_name, members) in &clusters_by_name {
            if cluster_name == "shell" {
                continue;
            }
            if closure.iter().any(|component| members.contains(component)) {
                names.insert(cluster_name.clone());
            }
        }
        route_plans.insert(route.component, names.into_iter().collect());
    }

    let mut clusters = BTreeMap::new();
    for (name, members) in clusters_by_name {
        let mut vtables = Vec::new();
        for member in members {
            let Some(vtable) = by_name.get(member).copied() else {
                return Err(ClusterLoadError::UnknownRouteComponent(member));
            };
            vtables.push(vtable);
        }
        clusters.insert(name, vtables);
    }

    Ok(InstalledManifest {
        clusters,
        route_plans,
        installed: BTreeSet::new(),
    })
}

fn collect_closure(
    root: &'static ComponentVTable,
    by_name: &HashMap<&'static str, &'static ComponentVTable>,
    out: &mut BTreeSet<&'static str>,
) -> Result<(), ClusterLoadError> {
    if !out.insert(root.name) {
        return Ok(());
    }
    for used in (root.uses)() {
        let Some(known) = by_name.get(used.name).copied() else {
            return Err(ClusterLoadError::MissingComponent {
                owner: root.name,
                missing: used.name,
            });
        };
        collect_closure(known, by_name, out)?;
    }
    Ok(())
}

#[doc(hidden)]
pub fn clear_cluster_manifest_for_test() {
    ACTIVE_MANIFEST.with(|active| {
        *active.borrow_mut() = None;
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::ComponentVTable;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn noop_register() {}
    fn no_uses() -> &'static [&'static ComponentVTable] {
        &[]
    }
    fn route_a_uses() -> &'static [&'static ComponentVTable] {
        static USES: &[&ComponentVTable] = &[&SHARED, &COMMON];
        USES
    }
    fn route_b_uses() -> &'static [&'static ComponentVTable] {
        static USES: &[&ComponentVTable] = &[&SHARED, &COMMON];
        USES
    }
    fn route_c_uses() -> &'static [&'static ComponentVTable] {
        static USES: &[&ComponentVTable] = &[&COMMON];
        USES
    }
    fn transitive_root_uses() -> &'static [&'static ComponentVTable] {
        static USES: &[&ComponentVTable] = &[&TRANSITIVE_MID];
        USES
    }
    fn transitive_mid_uses() -> &'static [&'static ComponentVTable] {
        static USES: &[&ComponentVTable] = &[&LEAF];
        USES
    }
    fn bundle_uses() -> &'static [&'static ComponentVTable] {
        static USES: &[&ComponentVTable] = &[&LEAF, &SHARED];
        USES
    }
    fn missing_uses() -> &'static [&'static ComponentVTable] {
        static USES: &[&ComponentVTable] = &[&MISSING];
        USES
    }

    static LEAF: ComponentVTable = ComponentVTable {
        name: "cluster-leaf",
        register: noop_register,
        uses: no_uses,
        is_bundle: false,
        template_html: None,
        plan: None,
        mount_template: None,
    };

    static SHARED: ComponentVTable = ComponentVTable {
        name: "cluster-shared",
        register: noop_register,
        uses: no_uses,
        is_bundle: false,
        template_html: None,
        plan: None,
        mount_template: None,
    };

    static COMMON: ComponentVTable = ComponentVTable {
        name: "cluster-common",
        register: noop_register,
        uses: no_uses,
        is_bundle: false,
        template_html: None,
        plan: None,
        mount_template: None,
    };

    static ROUTE_A: ComponentVTable = ComponentVTable {
        name: "cluster-route-a",
        register: noop_register,
        uses: route_a_uses,
        is_bundle: false,
        template_html: None,
        plan: None,
        mount_template: None,
    };

    static ROUTE_B: ComponentVTable = ComponentVTable {
        name: "cluster-route-b",
        register: noop_register,
        uses: route_b_uses,
        is_bundle: false,
        template_html: None,
        plan: None,
        mount_template: None,
    };

    static ROUTE_C: ComponentVTable = ComponentVTable {
        name: "cluster-route-c",
        register: noop_register,
        uses: route_c_uses,
        is_bundle: false,
        template_html: None,
        plan: None,
        mount_template: None,
    };

    static TRANSITIVE_ROOT: ComponentVTable = ComponentVTable {
        name: "cluster-transitive-root",
        register: noop_register,
        uses: transitive_root_uses,
        is_bundle: false,
        template_html: None,
        plan: None,
        mount_template: None,
    };

    static TRANSITIVE_MID: ComponentVTable = ComponentVTable {
        name: "cluster-transitive-mid",
        register: noop_register,
        uses: transitive_mid_uses,
        is_bundle: false,
        template_html: None,
        plan: None,
        mount_template: None,
    };

    static BUNDLE: ComponentVTable = ComponentVTable {
        name: "cluster-bundle",
        register: noop_register,
        uses: bundle_uses,
        is_bundle: true,
        template_html: None,
        plan: None,
        mount_template: None,
    };

    static MISSING: ComponentVTable = ComponentVTable {
        name: "cluster-missing",
        register: noop_register,
        uses: no_uses,
        is_bundle: false,
        template_html: None,
        plan: None,
        mount_template: None,
    };

    static OWNER_WITH_MISSING: ComponentVTable = ComponentVTable {
        name: "cluster-owner-with-missing",
        register: noop_register,
        uses: missing_uses,
        is_bundle: false,
        template_html: None,
        plan: None,
        mount_template: None,
    };

    fn cluster<'a>(clusters: &'a [Cluster], name: &str) -> &'a Cluster {
        clusters
            .iter()
            .find(|cluster| cluster.name == name)
            .unwrap()
    }

    #[test]
    fn cluster_plan_splits_shell_route_and_shared_ownership() {
        static COMPONENTS: &[&ComponentVTable] = &[&ROUTE_A, &ROUTE_B, &ROUTE_C, &SHARED, &COMMON];
        static ROUTES: &[RouteClusterRoot] = &[
            RouteClusterRoot {
                route_id: "r0",
                component: "cluster-route-a",
            },
            RouteClusterRoot {
                route_id: "r1",
                component: "cluster-route-b",
            },
            RouteClusterRoot {
                route_id: "r2",
                component: "cluster-route-c",
            },
        ];
        static MANIFEST: ClusterManifest = ClusterManifest {
            components: COMPONENTS,
            routes: ROUTES,
        };

        let clusters = cluster_plan_for_test(&MANIFEST).unwrap();
        assert_eq!(
            cluster(&clusters, "route:r0").components,
            ["cluster-route-a"]
        );
        assert_eq!(
            cluster(&clusters, "route:r1").components,
            ["cluster-route-b"]
        );
        assert_eq!(
            cluster(&clusters, "route:r2").components,
            ["cluster-route-c"]
        );
        assert_eq!(
            cluster(&clusters, "shared:r0+r1").components,
            ["cluster-shared"]
        );
        assert!(
            cluster(&clusters, "shell")
                .components
                .contains(&"cluster-common"),
            "component used by every route belongs to shell",
        );
    }

    #[test]
    fn transitive_uses_are_included_in_route_closure() {
        static COMPONENTS: &[&ComponentVTable] = &[&TRANSITIVE_ROOT, &TRANSITIVE_MID, &LEAF];
        static ROUTES: &[RouteClusterRoot] = &[RouteClusterRoot {
            route_id: "only",
            component: "cluster-transitive-root",
        }];
        static MANIFEST: ClusterManifest = ClusterManifest {
            components: COMPONENTS,
            routes: ROUTES,
        };

        let clusters = cluster_plan_for_test(&MANIFEST).unwrap();
        let shell = cluster(&clusters, "shell");
        assert!(shell.components.contains(&"cluster-transitive-root"));
        assert!(shell.components.contains(&"cluster-transitive-mid"));
        assert!(shell.components.contains(&"cluster-leaf"));
    }

    #[test]
    fn bundle_vtable_exposes_members_as_uses() {
        assert!(BUNDLE.is_bundle);
        assert_eq!(
            (BUNDLE.uses)().iter().map(|v| v.name).collect::<Vec<_>>(),
            ["cluster-leaf", "cluster-shared"]
        );
    }

    #[test]
    fn missing_transitive_component_is_a_manifest_error() {
        static COMPONENTS: &[&ComponentVTable] = &[&OWNER_WITH_MISSING];
        static ROUTES: &[RouteClusterRoot] = &[RouteClusterRoot {
            route_id: "only",
            component: "cluster-owner-with-missing",
        }];
        static MANIFEST: ClusterManifest = ClusterManifest {
            components: COMPONENTS,
            routes: ROUTES,
        };

        assert_eq!(
            cluster_plan_for_test(&MANIFEST),
            Err(ClusterLoadError::MissingComponent {
                owner: "cluster-owner-with-missing",
                missing: "cluster-missing",
            })
        );
    }

    static SHELL_REGISTER_COUNT: AtomicUsize = AtomicUsize::new(0);
    static ROUTE_REGISTER_COUNT: AtomicUsize = AtomicUsize::new(0);

    fn count_shell_register() {
        SHELL_REGISTER_COUNT.fetch_add(1, Ordering::SeqCst);
    }

    fn count_route_register() {
        ROUTE_REGISTER_COUNT.fetch_add(1, Ordering::SeqCst);
    }

    static COUNT_SHELL: ComponentVTable = ComponentVTable {
        name: "cluster-count-shell",
        register: count_shell_register,
        uses: no_uses,
        is_bundle: false,
        template_html: None,
        plan: None,
        mount_template: None,
    };

    static COUNT_ROUTE: ComponentVTable = ComponentVTable {
        name: "cluster-count-route",
        register: count_route_register,
        uses: no_uses,
        is_bundle: false,
        template_html: None,
        plan: None,
        mount_template: None,
    };

    static COUNT_OTHER_ROUTE: ComponentVTable = ComponentVTable {
        name: "cluster-count-other-route",
        register: noop_register,
        uses: no_uses,
        is_bundle: false,
        template_html: None,
        plan: None,
        mount_template: None,
    };

    #[test]
    fn loader_installs_shell_and_memoizes_route_clusters() {
        clear_cluster_manifest_for_test();
        SHELL_REGISTER_COUNT.store(0, Ordering::SeqCst);
        ROUTE_REGISTER_COUNT.store(0, Ordering::SeqCst);

        static COMPONENTS: &[&ComponentVTable] = &[&COUNT_SHELL, &COUNT_ROUTE, &COUNT_OTHER_ROUTE];
        static ROUTES: &[RouteClusterRoot] = &[
            RouteClusterRoot {
                route_id: "r0",
                component: "cluster-count-route",
            },
            RouteClusterRoot {
                route_id: "r1",
                component: "cluster-count-other-route",
            },
        ];
        static MANIFEST: ClusterManifest = ClusterManifest {
            components: COMPONENTS,
            routes: ROUTES,
        };

        install_cluster_manifest(&MANIFEST).unwrap();
        assert_eq!(SHELL_REGISTER_COUNT.load(Ordering::SeqCst), 1);
        assert_eq!(ROUTE_REGISTER_COUNT.load(Ordering::SeqCst), 0);

        ensure_route_component_ready("cluster-count-route").unwrap();
        ensure_route_component_ready("cluster-count-route").unwrap();
        assert_eq!(ROUTE_REGISTER_COUNT.load(Ordering::SeqCst), 1);
        assert_eq!(installed_clusters_for_test(), ["route:r0", "shell"]);
    }
}

//! Route artifact ABI surface.
//!
//! This is intentionally a descriptor shape, not a Rust component
//! vtable. Route crates use this type through `pocopine::route!` to
//! declare stable identity before the build pipeline lowers the route
//! into JS descriptors, compiled plans, SSR entries, or host-ABI
//! handler modules.

/// Current route artifact ABI version.
pub const ROUTE_ABI_VERSION: u32 = 1;

/// Static descriptor emitted by `pocopine::route!`.
///
/// The descriptor is safe to expose across crates because it contains
/// serializable identity only. It deliberately does not contain Rust
/// function pointers, constructors, `Scope`, or `ComponentVTable`
/// addresses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RouteDescriptor {
    pub abi_version: u32,
    pub id: &'static str,
    pub path: &'static str,
    pub component: &'static str,
    pub hydrate: bool,
    pub ssr: bool,
}

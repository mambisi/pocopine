---
title: "Dynamic components"
description: "Select a registered component reactively with a typed ComponentRef, forward props, preserve instances, and handle data-driven component names."
---

# Dynamic components

`<pp-component>` changes which component occupies one place in a template
without changing the URL. The selection is reactive, just like any other
binding.

Use a `ComponentRef` for selections made in Rust:

```rust
#[derive(Default, Serialize, Deserialize)]
#[component(
    uses = [ProfilePanel, BillingPanel],
    template = poco! {
        <pp-component :is="active" :account-id="account_id"></pp-component>
    },
)]
struct Settings {
    active: Option<ComponentRef<Settings>>,
    account_id: String,
}

#[handlers]
impl Settings {
    pub fn show_profile(&mut self) {
        self.active = Some(ComponentRef::of::<ProfilePanel>());
    }

    pub fn show_billing(&mut self) {
        self.active = Some(ComponentRef::of::<BillingPanel>());
    }
}
```

`ComponentRef::of::<Child>()` requires `Child: Component`. Rust infers `Host`
from the expected `ComponentRef<Host>` field or return type, then also proves
that `Host` declares `Child` in `uses`. The `#[component]` macro emits that
proof from the list above. Removing `BillingPanel` from `Settings::uses`, or
trying to select some other component, is therefore a Rust compile error. The
constructor registers the host dependency graph and gets both runtime
identities from the component types, so there is no component-name string to
mistype.

When no expected type is available, provide one or spell the host explicitly:

```rust
let selected: ComponentRef<Settings> = ComponentRef::of::<ProfilePanel>();
let selected = ComponentRef::<Settings>::of::<ProfilePanel>();
```

When a store produces the selection, put the outlet host in the computed
return type and constructor explicitly. `Self` there would mean the store, not
the outlet:

```rust
#[computed]
fn screen(route_key: &String) -> Option<ComponentRef<Settings>> {
    match route_key.as_str() {
        "profile" => Some(ComponentRef::of::<ProfilePanel>()),
        "billing" => Some(ComponentRef::of::<BillingPanel>()),
        _ => None,
    }
}
```

List statically reachable candidates in `uses`. This keeps the component
dependency graph visible and registers every candidate when the host is
registered. The host is part of the Rust type, so assigning a
`ComponentRef<OtherHost>` to this field is a compile error. The runtime retains
the host identity as a backstop for values arriving through dynamic store
paths.

`Component` is not exposed as `dyn Component`: its name and
registration/mount functions are type-level and the trait is intentionally not
object-safe. `ComponentRef<Host>` is the small child-erased token carried
through reactive state after the `(Host, Child)` pair has been checked.

## Selection and cleanup

`:is` is required. Changing it swaps the child; setting an
`Option<ComponentRef<Host>>` to `None` renders nothing. Without `keep-alive`, the
outgoing component runs normal unmount cleanup and selecting it again creates
fresh state.

Bound attributes other than `:is` are forwarded as child props. They are
evaluated before the first child mount, so `on_setup` sees their initial values,
and later changes update the mounted child's declared `#[prop]` fields without
remounting it.

```html
<pp-component
  :is="active"
  :account-id="account_id"
  :filters="filters"
></pp-component>
```

The values stay as `JsValue`s through the forwarding path; objects and arrays
are not reduced to display strings.

## Preserving state

Add `keep-alive` to cache each selected component by its canonical identity:

```html
<pp-component :is="active" keep-alive></pp-component>
```

Switching away hides the instance instead of unmounting it. Local state, DOM
identity, focusable-control state, and scroll position survive a round trip.
All cached instances still unmount when the owning `<pp-component>` leaves the
DOM.

Transition attributes on the sentinel are forwarded to each selected child:

```html
<pp-component
  :is="active"
  pp-transition:in="fade"
  pp-transition:out="fade"
></pp-component>
```

## Data-driven names

Plugins or server data may supply a name rather than a Rust type. Validate that
boundary explicitly:

```rust
let selected =
    ComponentRef::<Settings>::from_registered_name(&payload.component);
```

The host parameter still limits the boundary to `Settings::uses`; the result
is `None` unless the name or alias resolves to one of those candidates. Prefer
`ComponentRef::of::<Child>()` everywhere the candidate type is known and the
host is supplied by the expected type.

Raw names are not a selection API. A `String` field or `#[computed]` value used
for a locally visible `:is` fails compilation, and a raw string arriving
through a dynamic store path is rejected by the runtime. If a store truly owns
the selection, store an `Option<ComponentRef<OutletHost>>`, not the registered
tag name. Naming the outlet in the store's return type is what connects that
store-produced value to the outlet's `uses` contract.

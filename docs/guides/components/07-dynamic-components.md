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
    template_inline = r#"
        <pp-component :is="active" :account-id="account_id"></pp-component>
    "#,
)]
struct Settings {
    active: Option<ComponentRef>,
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

`ComponentRef::of::<C>()` requires `C: Component`, registers the component,
and gets the runtime identity from `C::NAME`. The authored code therefore has
no component-name string to mistype. `Component` is not exposed as
`dyn Component`: its name and registration/mount functions are type-level and
the trait is intentionally not object-safe.

List statically reachable candidates in `uses`. This keeps the component
dependency graph visible and registers every candidate when the host is
registered.

## Selection and cleanup

`:is` is required. Changing it swaps the child; setting an
`Option<ComponentRef>` to `None` renders nothing. Without `keep-alive`, the
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
let selected = ComponentRef::from_registered_name(&payload.component);
```

The result is `None` unless the name or alias already resolves to a registered
component. Prefer `ComponentRef::of::<C>()` everywhere the candidate type is
known at compile time.

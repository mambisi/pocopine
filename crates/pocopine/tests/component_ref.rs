//! Host-side contract coverage for typed dynamic-component references.

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(name = "dc-ref-allowed", template_inline = "<p>allowed</p>")]
struct AllowedChild {}

#[handlers]
impl AllowedChild {}

#[derive(Default, Serialize, Deserialize)]
#[component(name = "dc-ref-unlisted", template_inline = "<p>unlisted</p>")]
struct UnlistedChild {}

#[handlers]
impl UnlistedChild {}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "dc-ref-host",
    uses = [AllowedChild],
    template_inline = r#"<pp-component :is="active"></pp-component>"#,
)]
struct RefHost {
    active: Option<ComponentRef<RefHost>>,
}

#[handlers]
impl RefHost {}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "dc-ref-other-host",
    uses = [AllowedChild],
    template_inline = "<div>other host</div>",
)]
struct OtherHost {}

#[handlers]
impl OtherHost {}

#[test]
fn typed_reference_round_trips_for_its_declared_host() {
    let selected: ComponentRef<RefHost> = ComponentRef::of::<AllowedChild>();
    let encoded = serde_json::to_value(selected).unwrap();
    let decoded = serde_json::from_value::<ComponentRef<RefHost>>(encoded).unwrap();

    assert_eq!(decoded, selected);
    assert_eq!(decoded.host(), RefHost::NAME);
    assert_eq!(decoded.name(), AllowedChild::NAME);
}

#[test]
fn registered_name_is_limited_to_the_host_uses_list() {
    UnlistedChild::register();

    assert!(ComponentRef::<RefHost>::from_registered_name(AllowedChild::NAME).is_some());
    assert!(ComponentRef::<RefHost>::from_registered_name(UnlistedChild::NAME).is_none());
}

#[test]
fn serialized_reference_cannot_change_hosts() {
    OtherHost::register();
    let selected: ComponentRef<RefHost> = ComponentRef::of::<AllowedChild>();
    let encoded = serde_json::to_value(selected).unwrap();

    assert!(serde_json::from_value::<ComponentRef<OtherHost>>(encoded).is_err());
}

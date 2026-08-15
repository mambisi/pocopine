use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = poco! { <p>allowed</p> })]
struct DcAllowedChild {}

#[handlers]
impl DcAllowedChild {}

#[derive(Default, Serialize, Deserialize)]
#[component(template = poco! { <p>not listed</p> })]
struct DcUnlistedChild {}

#[handlers]
impl DcUnlistedChild {}

#[derive(Default, Serialize, Deserialize)]
#[component(
    uses = [DcAllowedChild],
    template = poco! {<pp-component :is="active"></pp-component>},
)]
struct DcUsesHost {
    active: Option<ComponentRef<DcUsesHost>>,
}

#[handlers]
impl DcUsesHost {
    pub fn select_unlisted(&mut self) {
        self.active = Some(ComponentRef::of::<DcUnlistedChild>());
    }
}

fn main() {}

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = poco! { <p>child</p> })]
struct DcTypedChild {}

#[handlers]
impl DcTypedChild {}

#[derive(Default, Serialize, Deserialize)]
#[component(
    uses = [DcTypedChild],
    template = poco! {<pp-component :is="active"></pp-component>},
)]
struct DcTypedHost {
    active: Option<ComponentRef<DcTypedHost>>,
}

#[handlers]
impl DcTypedHost {
    pub fn select(&mut self) {
        self.active = Some(ComponentRef::of::<DcTypedChild>());
    }
}

fn main() {}

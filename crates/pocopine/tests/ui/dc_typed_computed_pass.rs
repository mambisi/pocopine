use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = poco! { <p>computed child</p> })]
struct DcComputedChild {}

#[handlers]
impl DcComputedChild {}

#[derive(Default, Serialize, Deserialize)]
#[component(
    uses = [DcComputedChild],
    template = poco! {<pp-component :is="active"></pp-component>},
)]
struct DcTypedComputedHost {
    selected: bool,
}

#[handlers]
impl DcTypedComputedHost {
    #[computed]
    fn active(selected: &bool) -> Option<ComponentRef<DcTypedComputedHost>> {
        selected.then(ComponentRef::of::<DcComputedChild>)
    }
}

fn main() {}

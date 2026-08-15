use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = poco! { <p>child</p> })]
struct DcSharedChild {}

#[handlers]
impl DcSharedChild {}

#[derive(Default, Serialize, Deserialize)]
#[component(uses = [DcSharedChild], template = poco! { <div>source</div> })]
struct DcSourceHost {}

#[handlers]
impl DcSourceHost {}

#[derive(Default, Serialize, Deserialize)]
#[component(
    uses = [DcSharedChild],
    template = poco! {<pp-component :is="active"></pp-component>},
)]
struct DcTargetHost {
    active: Option<ComponentRef<DcSourceHost>>,
}

#[handlers]
impl DcTargetHost {}

fn main() {}

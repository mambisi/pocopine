use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template_inline = "<p>child</p>")]
struct DcSharedChild {}

#[handlers]
impl DcSharedChild {}

#[derive(Default, Serialize, Deserialize)]
#[component(uses = [DcSharedChild], template_inline = "<div>source</div>")]
struct DcSourceHost {}

#[handlers]
impl DcSourceHost {}

#[derive(Default, Serialize, Deserialize)]
#[component(
    uses = [DcSharedChild],
    template_inline = r#"<pp-component :is="active"></pp-component>"#,
)]
struct DcTargetHost {
    active: Option<ComponentRef<DcSourceHost>>,
}

#[handlers]
impl DcTargetHost {}

fn main() {}

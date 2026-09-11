use pocopine::prelude::*;

#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
struct Errors(Vec<String>);

#[derive(Default, serde::Serialize, serde::Deserialize)]
#[component(template = poco! { <div></div> })]
struct Editor {
    start: String,
    end: String,
    errors: Errors,
    valid: bool,
}

#[handlers]
impl Editor {
    // Parameters bind by name, not position. Outputs can have private types.
    #[watch(start, end, writes(errors, valid))]
    fn check(end: Change<String>, start: Change<String>) -> Update<Self> {
        let valid = start.current <= end.current;
        Update::new().errors(Errors::default()).valid(valid)
    }

    #[watch(valid)]
    fn log(valid: Change<bool>) { let _ = valid.changed(); }
}

fn main() {}

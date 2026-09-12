#![deny(warnings)]
use pocopine::prelude::*;

mod model {
    use super::*;
    #[derive(Default, serde::Serialize, serde::Deserialize)]
    #[store(name = "explicit")]
    pub struct Editor {
        pub record: String,
        pub draft: String,
        pub dirty: bool,
        pub error: Option<String>,
    }
    #[handlers]
    impl Editor {
        #[watch(record)]
        pub fn explicit(record: &str) -> Update<Self, (Self::Draft, Self::Dirty, Self::Error)> {
            super::make_patch(record)
        }
        #[watch(record, updates(draft, dirty, error))]
        pub fn sugar(record: &str) -> Update<Self> {
            Self::explicit(record)
        }
        #[watch(dirty)]
        fn observer(dirty: bool) { let _ = dirty; }
    }
}

// Reusable marker types and setters work outside both the owner module and
// #[handlers]. The explicit and sugar functions have the same Rust type.
fn make_patch(record: &str) -> Update<model::Editor, (model::EditorField::Draft, model::EditorField::Dirty, model::EditorField::Error)> {
    use model::EditorField::setters::{Draft as _, Dirty as _, Error as _};
    Update::new().draft(record.to_owned()).dirty(false).error(None)
}
fn main() {
    let owner = model::Editor::default();
    let _: &String = model::EditorField::Draft::get(&owner);
    assert_eq!(<model::EditorField::Draft as Field<model::Editor>>::NAME, "draft");
    let _ = if true { model::Editor::explicit("a") } else { model::Editor::sugar("b") };
}

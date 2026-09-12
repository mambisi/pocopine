#![deny(warnings)]
use pocopine::prelude::*;

mod theme {
    #[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
    pub struct Theme { pub value: u32 }
}

mod config {
    #[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
    pub struct Config { pub value: u32 }
}

// Field metadata is generated even without watchers.
#[derive(Default, serde::Serialize, serde::Deserialize)]
#[component(name = "field-module-shadow", template = poco! { <div></div> })]
struct PlainComponent { theme: theme::Theme, config: config::Config }

#[handlers]
impl PlainComponent {}

#[derive(Default, serde::Serialize, serde::Deserialize)]
#[store(name = "field-module-shadow-store")]
struct PlainStore { theme: theme::Theme, config: config::Config }

#[handlers]
impl PlainStore {}

mod nested {
    use super::*;

    #[derive(Default, serde::Serialize, serde::Deserialize)]
    #[store(name = "field-module-shadow-watched")]
    pub struct Watched {
        theme: self::theme::Theme,
        config: super::config::Config,
    }

    #[handlers]
    impl Watched {
        #[watch(theme, writes(config))]
        fn configure(theme: &theme::Theme) -> Update<Self> {
            Update::new().config(config::Config { value: theme.value })
        }

        #[watch]
        pub fn observe(changes: Changes<Self>) {
            let _: theme::Theme = changes.theme.current;
            let _: Option<config::Config> = changes.config.previous;
        }
    }
}

fn main() {}

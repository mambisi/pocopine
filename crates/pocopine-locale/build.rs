use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rustc-check-cfg=cfg(pocopine_locale_data)");
    println!("cargo:rerun-if-env-changed=POCOPINE_LOCALE_DATA_DIR");
    println!("cargo:rerun-if-changed=src/generated.rs");
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo OUT_DIR"));
    let source = if let Some(directory) = env::var_os("POCOPINE_LOCALE_DATA_DIR") {
        let directory = PathBuf::from(directory);
        let rules = directory.join("plural.rs");
        let data = directory.join("formatting.blob");
        for path in [&rules, &data] {
            println!("cargo:rerun-if-changed={}", path.display());
        }
        fs::copy(&data, output.join("formatting.blob"))
            .expect("locale ICU data missing; rebuild this application with the Pocopine CLI");
        println!("cargo:rustc-cfg=pocopine_locale_data");
        fs::read_to_string(rules)
            .expect("locale plural data missing; rebuild this application with the Pocopine CLI")
    } else {
        fs::read_to_string("src/generated.rs").expect("checked-in CLDR predicates")
    };
    // The containing module owns lint attributes for both checked-in and
    // configured code; inner attributes cannot be expanded through include!.
    let source = source
        .lines()
        .filter(|line| !line.starts_with("#![allow("))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(output.join("plural_rules.rs"), source).expect("write generated plural predicates");
}

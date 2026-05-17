fn main() {
    pocopine_client_build::generate().expect("generate Pocopine client module bindings");

    println!("cargo:rustc-check-cfg=cfg(pocopine_browser)");
    println!("cargo:rustc-check-cfg=cfg(pocopine_host)");

    match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("wasm32") => println!("cargo:rustc-cfg=pocopine_browser"),
        _ => println!("cargo:rustc-cfg=pocopine_host"),
    }
}

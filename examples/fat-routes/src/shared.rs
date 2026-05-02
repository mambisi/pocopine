pub fn route_seed(label: &str) -> u32 {
    let now = js_sys::Date::now() as u32;
    label.bytes().fold(now ^ 0x9e37_79b9, |acc, byte| {
        acc.rotate_left(5) ^ byte as u32
    })
}

pub fn summarize(label: &str, score: u32) -> String {
    format!("{label} route score: {score}")
}

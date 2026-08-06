use pocopine::{Handle, dispatch};

#[derive(Default)]
struct Target {
    value: u32,
}

// Compile-time coverage for the explicit Handle<T> form. Keeping this as a
// function (rather than running it on the host) exercises the public macro
// expansion without requiring the browser's microtask executor.
#[allow(dead_code)]
fn dispatch_to_explicit_handle(handle: Handle<Target>) {
    dispatch!(handle, async { 41_u32 }.await, |target, result| {
        target.value = result + 1;
    });
}

// The original implicit-this form remains source-compatible.
impl Target {
    #[allow(dead_code)]
    fn dispatch_to_self() {
        dispatch!(async { 41_u32 }.await, |target, result| {
            target.value = result + 1;
        });
    }
}

#[test]
fn dispatch_forms_type_check() {
    // The useful assertion is compilation of both functions above. Keep one
    // ordinary test so Cargo always builds this integration-test target.
    assert_eq!(std::mem::size_of::<Target>(), std::mem::size_of::<u32>());
}

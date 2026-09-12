//! Compile-time contract for `#[handlers]` marker attributes.
//!
//! Watchers have named snapshot inputs, declared patch outputs, and a checked
//! dependency graph. Invalid declarations must fail at compile time.
//!
//! Host-only: trybuild shells out to cargo and isn't a wasm target.
#![cfg(not(target_arch = "wasm32"))]

#[test]
fn handlers_marker_contract() {
    let cases = trybuild::TestCases::new();
    cases.pass("tests/ui/watch_pass.rs");
    cases.pass("tests/ui/watch_multi_pass.rs");
    cases.compile_fail("tests/ui/watch_no_args.rs");
    cases.compile_fail("tests/ui/watch_one_arg.rs");
    cases.compile_fail("tests/ui/watch_bare_attr.rs");
    cases.compile_fail("tests/ui/watch_nonident_attr.rs");
    cases.compile_fail("tests/ui/watch_stacked.rs");
    cases.compile_fail("tests/ui/watch_no_receiver.rs");
    cases.compile_fail("tests/ui/watch_mut_receiver.rs");
    cases.compile_fail("tests/ui/watch_multi_mut_receiver.rs");
    cases.compile_fail("tests/ui/watch_mutation.rs");
    cases.compile_fail("tests/ui/lifecycle_no_receiver.rs");
    cases.compile_fail("tests/ui/computed_with_args.rs");
    cases.compile_fail("tests/ui/watch_multi_with_args.rs");
    cases.compile_fail("tests/ui/watch_list_duplicate.rs");
    cases.compile_fail("tests/ui/watch_self_write.rs");
    cases.compile_fail("tests/ui/watch_cycle.rs");
    cases.compile_fail("tests/ui/watch_forbidden_setter.rs");
    cases.compile_fail("tests/ui/watch_wrong_payload.rs");
    cases.compile_fail("tests/ui/watch_missing_input.rs");
    cases.compile_fail("tests/ui/watch_bad_return.rs");
    cases.compile_fail("tests/ui/watch_async.rs");
    cases.pass("tests/ui/watch_cfg_raw_pass.rs");
    cases.compile_fail("tests/ui/watch_input_type.rs");
    cases.compile_fail("tests/ui/watch_unknown_output.rs");
    cases.pass("tests/ui/watch_forms_pass.rs");
    cases.pass("tests/ui/watch_all_pass.rs");
    cases.pass("tests/ui/watch_field_module_shadow_pass.rs");
    cases.compile_fail("tests/ui/watch_mut_input.rs");
    cases.compile_fail("tests/ui/watch_all_patch.rs");
    cases.compile_fail("tests/ui/watch_all_input.rs");
    cases.compile_fail("tests/ui/watch_all_writes.rs");
    cases.pass("tests/ui/watch_explicit_pass.rs");
    cases.pass("tests/ui/watch_empty_allowed_pass.rs");
    cases.compile_fail("tests/ui/watch_empty_tuple.rs");
    cases.compile_fail("tests/ui/watch_empty_updates.rs");
    cases.compile_fail("tests/ui/watch_outputs_missing.rs");
    cases.compile_fail("tests/ui/watch_outputs_both.rs");
    cases.compile_fail("tests/ui/watch_explicit_duplicate.rs");
    cases.compile_fail("tests/ui/watch_explicit_self_write.rs");
    cases.compile_fail("tests/ui/watch_explicit_unknown.rs");
    cases.compile_fail("tests/ui/watch_explicit_not_tuple.rs");
    cases.compile_fail("tests/ui/watch_legacy_writes.rs");
    cases.compile_fail("tests/ui/watch_empty_all_patch.rs");
    cases.compile_fail("tests/ui/watch_updates_unit.rs");
    cases.compile_fail("tests/ui/watch_explicit_cycle.rs");

    cases.pass("tests/ui/watch_large_tuple_pass.rs");
    cases.compile_fail("tests/ui/watch_tuple_too_large.rs");
    cases.compile_fail("tests/ui/watch_marker_collision.rs");
    cases.compile_fail("tests/ui/watch_marker_visibility.rs");
}

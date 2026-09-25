#[test]
fn display_paths_cannot_be_used_as_operation_targets() {
    let tests = trybuild::TestCases::new();
    tests.compile_fail("tests/ui/display_path_is_not_store_path.rs");
}

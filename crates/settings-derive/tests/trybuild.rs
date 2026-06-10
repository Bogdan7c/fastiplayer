#[test]
fn settings_schema_trybuild_contracts() {
    let test_cases = trybuild::TestCases::new();
    test_cases.compile_fail("tests/ui/missing_metadata.rs");
    test_cases.compile_fail("tests/ui/unknown_editor.rs");
    test_cases.pass("tests/pass/read_only_schema_version.rs");
    test_cases.pass("tests/pass/nested_registry.rs");
}

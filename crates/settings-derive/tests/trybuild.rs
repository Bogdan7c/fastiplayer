// Интеграционный тест целиком является тестовым кодом: unwrap/expect/panic
// здесь работают как assertions. Production-политика паник сюда не относится.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]

#[test]
fn settings_schema_trybuild_contracts() {
    let test_cases = trybuild::TestCases::new();
    test_cases.compile_fail("tests/ui/missing_metadata.rs");
    test_cases.compile_fail("tests/ui/unknown_editor.rs");
    test_cases.pass("tests/pass/read_only_schema_version.rs");
    test_cases.pass("tests/pass/nested_registry.rs");
}

#[test]
fn non_main_sequences_are_not_native_functions() {
    let cases = trybuild::TestCases::new();
    cases.compile_fail("tests/ui/non_main_sequence_native_call.rs");
}

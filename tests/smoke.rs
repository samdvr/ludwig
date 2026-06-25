#[test]
fn version_is_set() {
    assert!(!ludwig::VERSION.is_empty());
    assert_eq!(ludwig::VERSION, "0.1.0");
}

/// Functional tests for skadi-core.
///
/// These tests exercise multi-component workflows and complex scenarios
/// that go beyond single-unit or API-level integration tests.
/// They validate that components work together correctly but do NOT
/// require external services (databases, APIs, etc.).
///
/// Tests requiring external services should use `#[ignore]` and will
/// be run separately in CI.

#[test]
fn functional_placeholder() {
    // Replace with real functional tests.
}

/// Example of a gated test that requires external services.
/// Run with: cargo test --test functional -- --ignored
#[test]
#[ignore]
fn requires_external_service() {
    // Tests that need databases, APIs, or other external services
    // should be marked #[ignore] and run separately.
}

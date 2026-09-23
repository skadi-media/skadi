Feature: Persistence — backend selection and embedded migrations (C02)
  One Store wraps a diesel-dualdb pool for SQLite (zero-config) or Postgres.
  Migrations for both backends are generated from one logical DDL, embedded
  in the binary and applied at every startup — Sonarr/Radarr's migrate-on-boot.

  @C02 @passing
  Scenario: a fresh SQLite database receives every embedded migration once
    Given a fresh sqlite database
    When the embedded migrations are applied
    Then every embedded migration was applied
    And every skadi-store table exists
    When the embedded migrations are applied
    Then the second application applied no migrations

  @C02 @passing
  Scenario: migrations revert and re-apply symmetrically on SQLite
    Given a migrated sqlite store
    When the embedded migrations are reverted
    Then no skadi-store table remains
    When the embedded migrations are applied
    Then the revert and the re-apply moved the same number of migrations
    And every skadi-store table exists

  @C02 @passing @postgres
  Scenario: a fresh Postgres database receives every embedded migration once
    Given a fresh postgres database
    When the embedded migrations are applied
    Then every embedded migration was applied
    And every skadi-store table exists
    When the embedded migrations are applied
    Then the second application applied no migrations

  @C02 @passing @postgres
  Scenario: migrations revert and re-apply symmetrically on Postgres
    Given a migrated postgres store
    When the embedded migrations are reverted
    Then no skadi-store table remains
    When the embedded migrations are applied
    Then the revert and the re-apply moved the same number of migrations

  @C02 @passing
  Scenario: the backend is selected by URL scheme and unknown schemes are refused
    Given a migrated sqlite store
    Then the store reports backend "sqlite"
    And connecting to "postgres://skadi:skadi@127.0.0.1:1/skadi" selects backend "postgres" without touching the network
    And connecting to "postgresql://127.0.0.1:1/skadi" selects backend "postgres" without touching the network
    And connecting to "mysql://localhost/skadi" is rejected as a configuration error

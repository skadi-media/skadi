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

  # ---- applied schema version vs the binary (SKADI-T-0682) -------------------
  # The health check `database` reads Store::schema_status: "behind" = an
  # embedded migration the database has not applied, "ahead" = an applied
  # migration this binary does not embed.

  @C02 @passing
  Scenario: a migrated SQLite database matches the embedded migrations
    Given a migrated sqlite store
    Then the schema status matches the embedded migrations

  @C02 @passing @postgres
  Scenario: a migrated Postgres database matches the embedded migrations
    Given a migrated postgres store
    Then the schema status matches the embedded migrations

  @C02 @passing
  Scenario: a database that was never migrated has every embedded migration pending
    Given a fresh sqlite database
    Then the schema status lists every embedded migration as pending
    And no skadi-store table remains

  @C02 @passing
  Scenario: a SQLite database without the newest migration is behind the binary
    Given a migrated sqlite store
    When the record of the newest embedded migration is removed from the database
    Then the schema status lists the newest embedded migration as pending
    And the schema status lists no unknown migration

  @C02 @passing @postgres
  Scenario: a Postgres database without the newest migration is behind the binary
    Given a migrated postgres store
    When the record of the newest embedded migration is removed from the database
    Then the schema status lists the newest embedded migration as pending
    And the schema status lists no unknown migration

  @C02 @passing
  Scenario: a SQLite database migrated by a newer binary is ahead of it
    Given a migrated sqlite store
    When the database records the migration "29991231000000" that this binary does not embed
    Then the schema status lists "29991231000000" as unknown
    And the schema status lists no pending migration

  @C02 @passing @postgres
  Scenario: a Postgres database migrated by a newer binary is ahead of it
    Given a migrated postgres store
    When the database records the migration "29991231000000" that this binary does not embed
    Then the schema status lists "29991231000000" as unknown
    And the schema status lists no pending migration

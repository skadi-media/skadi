Feature: Domain registry & supervisor — daemon bootstrap (C06)
  Boot order: ensure the cloacina database (Postgres only) -> skadi-store
  migrations -> seed env into the config table -> a disabled domains row per
  compiled-in module -> each domain's migrations -> one-time mode presets ->
  first-boot provisioning (first-boot.feature). Idempotent on every restart.

  Background:
    Given a daemon running in open mode

  @C06 @passing
  Scenario: a fresh bootstrap registers the compiled-in domain disabled and applies its schema
    When the daemon bootstraps with the "probe" domain compiled in
    Then the domain "probe" is registered but disabled
    And the table "bootstrap_probe" exists
    And the table "settings" exists
    And the config key "bootstrap.presets_seeded" equals "true"
    And the profiles settings kind holds at least 1 document
    And the custom_formats settings kind holds at least 1 document
    And exactly 1 built-in downloader is configured

  @C06 @passing
  Scenario: bootstrapping again is a no-op that never clobbers operator edits
    When the daemon bootstraps with the "probe" domain compiled in
    And the operator deletes one profiles document
    And the daemon bootstraps with the "probe" domain compiled in
    Then the domain "probe" is registered but disabled
    And exactly 1 built-in downloader is configured
    And the profiles settings kind holds at least 1 document

  @C06 @passing
  Scenario: an operator-enabled domain stays enabled across a restart
    Given the daemon compiles in the "probe" domain
    When the daemon bootstraps with the "probe" domain compiled in
    And the "probe" domain is enabled
    And the daemon bootstraps with the "probe" domain compiled in
    And the client requests GET "/api/v1/domains"
    Then the response status is 200
    And the response field "0.name" is "probe"
    And the response field "0.enabled" is "true"

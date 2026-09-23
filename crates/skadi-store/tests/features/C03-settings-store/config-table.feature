Feature: Settings store — the config table and its change fingerprint (C03)
  The `config` table is the runtime source of truth for process configuration.
  Env values are written at boot and always win; runtime values come from the
  API between boots. The supervisor polls a fingerprint of the table every
  tick to decide whether anything changed.

  @C03 @passing
  Scenario: an env write overwrites a runtime value, and provenance is recorded
    Given a migrated sqlite store
    When the config key "mode" is set to "just-go" from runtime
    Then the config key "mode" reads "just-go" from runtime
    When the config key "mode" is set to "production" from env
    Then the config key "mode" reads "production" from env
    And the config table holds 1 row in key order "mode"

  @C03 @passing
  Scenario: keys list in order, delete reports whether a row existed
    Given a migrated sqlite store
    When the config key "mode" is set to "testing" from env
    And the config key "bind_addr" is set to "0.0.0.0:9000" from env
    Then the config table holds 2 rows in key order "bind_addr, mode"
    When the config key "mode" is deleted
    Then the delete reported true
    And the config key "mode" is absent
    When the config key "mode" is deleted
    Then the delete reported false

  @C03 @passing
  Scenario: the fingerprint is stable across reads and moves on a value change or delete
    Given a migrated sqlite store
    And the config key "mode" is set to "testing" from env
    And the config fingerprint is taken
    When the config fingerprint is taken
    Then the fingerprint is unchanged
    When the config key "mode" is set to "production" from runtime
    And the config fingerprint is taken
    Then the fingerprint changed
    When the config key "mode" is deleted
    And the config fingerprint is taken
    Then the fingerprint changed

  @C03 @bug @SKADI-T-0459
  Scenario: re-writing an unchanged value is not a change
    Given a migrated sqlite store
    And the config key "naming.space" is set to "_" from runtime
    And the config fingerprint is taken
    When the config key "naming.space" is re-written with the same value "_"
    And the config fingerprint is taken
    Then the fingerprint is unchanged

  @C03 @passing @postgres
  Scenario: env-over-runtime and the fingerprint behave the same on Postgres
    Given a migrated postgres store
    When the config key "mode" is set to "just-go" from runtime
    And the config key "mode" is set to "production" from env
    Then the config key "mode" reads "production" from env
    When the config fingerprint is taken
    And the config fingerprint is taken
    Then the fingerprint is unchanged
    When the config key "mode" is set to "testing" from runtime
    And the config fingerprint is taken
    Then the fingerprint changed

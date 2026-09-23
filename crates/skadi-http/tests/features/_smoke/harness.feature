Feature: BDD harness for skadi-http
  The runner, world and tag filter work (SKADI-I-0057 P0).

  @C00 @passing
  Scenario: the harness runs a scenario
    Given the BDD harness for this crate
    When a scenario runs
    Then it passes

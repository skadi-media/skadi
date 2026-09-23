Feature: C30 settings hot reload
  The supervisor fingerprints provider + profile settings and the config table
  every tick and re-publishes a rebuilt provider set to every registered
  domain reloader when the fingerprint changes (no restart to reconfigure).

  Background:
    Given a daemon running in open mode

  @C30 @passing
  Scenario: the first reconcile applies whatever is stored, later idle ticks do nothing
    Given 2 provider reloaders are registered with the supervisor
    When the supervisor reconciles
    Then every reloader has been applied 1 time
    When the supervisor reconciles
    Then every reloader has been applied 1 time

  @C30 @passing
  Scenario: a provider settings change is applied exactly once to each reloader
    Given 2 provider reloaders are registered with the supervisor
    When the supervisor reconciles
    And the client sends POST "/api/v1/settings/indexers" with body:
      """
      { "kind": "torznab", "name": "ixr", "base_url": "http://127.0.0.1:1", "categories": [2000], "api_key": "k" }
      """
    And the supervisor reconciles
    Then every reloader has been applied 2 times
    When the supervisor reconciles
    Then every reloader has been applied 2 times

  @C30 @passing
  Scenario: a quality profile change also triggers a reload
    Given 1 provider reloaders are registered with the supervisor
    When the supervisor reconciles
    And the client sends POST "/api/v1/settings/profiles" with body:
      """
      { "name": "HD" }
      """
    And the supervisor reconciles
    Then every reloader has been applied 2 times

  @C30 @bug @SKADI-T-0459
  Scenario: an unrelated config write does not rebuild the provider set on every domain
    Given 1 provider reloaders are registered with the supervisor
    When the supervisor reconciles
    And the config key "naming.space" is set to "-"
    And the supervisor reconciles
    Then every reloader has been applied 1 time

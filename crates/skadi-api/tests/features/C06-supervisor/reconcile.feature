Feature: Domain registry & supervisor — worker lifecycle and reload errors (C06)
  Every 5 s the supervisor first reconciles the provider set (fingerprint
  change -> rebuild + apply to every domain reloader) and then the domain
  workers: enabled-but-idle domains get their workers spawned; disabled-but-
  running domains are cancelled and awaited before the entry is dropped.

  Background:
    Given a daemon running in open mode

  @C06 @passing
  Scenario: enabling a domain starts its workers; disabling cancels and awaits them; re-enabling respawns
    Given the supervisor supervises the "probe" domain whose worker waits for cancellation
    When the supervisor reconciles
    Then 0 domains are running
    When the "probe" domain is enabled
    And the supervisor reconciles
    Then 1 domain is running
    And the "probe" worker has started 1 time and stopped 0 times
    When the supervisor reconciles
    Then the "probe" worker has started 1 time and stopped 0 times
    When the "probe" domain is disabled
    And the supervisor reconciles
    Then 0 domains are running
    And the "probe" worker has started 1 time and stopped 1 time
    When the "probe" domain is enabled
    And the supervisor reconciles
    Then the "probe" worker has started 2 times and stopped 1 time
    When the supervisor shuts down
    Then 0 domains are running
    And the "probe" worker has started 2 times and stopped 2 times

  @C06 @passing @SKADI-T-0519
  Scenario: a domain whose provider reload fails must not stop every other domain's workers from starting
    Given the supervisor supervises the "probe" domain whose worker waits for cancellation
    And a provider reloader that always fails is registered with the supervisor
    And the "probe" domain is enabled
    When the supervisor reconciles, tolerating an error
    Then the reconcile failed mentioning "provider apply failed"
    And 1 domain is running

  @C06 @passing @SKADI-T-0523
  Scenario: a worker that exits on its own is restarted or at least reported as not running
    Given the supervisor supervises the "probe" domain whose worker exits immediately
    And the "probe" domain is enabled
    When the supervisor reconciles
    Then the "probe" worker has started 1 time and stopped 1 time
    When the supervisor reconciles
    Then the "probe" domain is restarted or reported as not running

  @C06 @passing @serial
  Scenario: a provider settings reload re-publishes services but does not by itself request a hunter sweep
    Given 1 provider reloaders are registered with the supervisor
    And a subscription to the hunter sweep trigger
    When the supervisor reconciles
    And the client sends POST "/api/v1/settings/indexers" with body:
      """
      { "kind": "torznab", "name": "ixr", "base_url": "http://127.0.0.1:1", "categories": [2000], "api_key": "k" }
      """
    And the supervisor reconciles
    Then every reloader has been applied 2 times
    And the hunter sweep trigger has not been poked

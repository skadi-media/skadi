Feature: C34 the Test button of a provider is its health check, run on its own
  The Indexers page shows, on each card, when the indexer was last tested and
  the result, also after a reload (SKADI-T-0699). That is the cached health
  check `indexer:<name>` (SKADI-T-0680): the Test button runs that one check
  with `POST /health/checks/run?id=…`, so the card and the health check never
  disagree. A check run on its own tests the provider live, also when its
  search circuit is open: after the operator fixes an indexer, Test must try
  it again instead of repeating the old error.

  Background:
    Given a daemon running in open mode

  @C34 @passing
  Scenario: a test of one indexer is stored, and a later read still shows it
    Given an indexer "testixr" whose server counts its requests
    When the health check "indexer:testixr" is run on its own
    Then the response status is 200
    And the server of indexer "testixr" has received 1 requests
    When the client requests GET "/api/v1/health/checks"
    Then the health check "indexer:testixr" has severity "ok"
    And the health check "indexer:testixr" has a checked_at time
    And the checked_at time of the health check "indexer:testixr" is remembered as "tested"
    When the client requests GET "/api/v1/health/checks"
    Then the health check "indexer:testixr" was checked at "tested"
    And the server of indexer "testixr" has received 1 requests

  @C34 @passing
  Scenario: a run of all checks trusts an open circuit, a run of one indexer tests it live
    Given an indexer "openixr" whose server counts its requests
    And 3 of the last 3 searches of indexer "openixr" failed
    When the health checks have run
    And the client requests GET "/api/v1/health/checks"
    Then the health check "indexer:openixr" has severity "error"
    And the server of indexer "openixr" has received 0 requests
    When the health check "indexer:openixr" is run on its own
    Then the server of indexer "openixr" has received 1 requests
    And the health check "indexer:openixr" has severity "warn"
    And the health check "indexer:openixr" message contains "reachable"

  @C34 @passing
  Scenario: a run of a check that does not exist is a 404
    When the health check "indexer:nope" is run on its own
    Then the response status is 404

Feature: C34 secrets are masked in health check messages and served log lines
  A provider error can carry the request URL, and a Torznab URL carries the
  indexer's API key: `error sending request for url (http://…/api?t=caps&apikey=KEY)`.
  `/health/checks` and `/log` are read by people and screenshots, so the
  daemon masks secrets in the text it serves (SKADI-T-0685): the value of a
  secret query key, the password in a URL, and the value of each configured
  provider secret wherever it appears. A masked value reads as `***`.

  Background:
    Given a daemon running in open mode

  @C34 @passing
  Scenario: the API key in the error of an unreachable indexer is masked in /health/checks
    Given an indexer "Leaky" at an address that refuses connections, with the API key "leaky-api-key-0685"
    When the health checks have run
    And the client requests GET "/api/v1/health/checks"
    Then the response status is 200
    And the health check "indexer:Leaky" has severity "error"
    And the health check "indexer:Leaky" message contains "apikey=***"
    And the response body does not contain "leaky-api-key-0685"

  @C34 @passing
  Scenario: the forced run answers with the key masked too
    Given an indexer "Leaky" at an address that refuses connections, with the API key "leaky-api-key-0685"
    When the client requests POST "/api/v1/health/checks/run"
    Then the response status is 200
    And the response body does not contain "leaky-api-key-0685"

  @C34 @passing
  Scenario: a logged keyed URL and a configured secret are masked in /log
    Given an indexer "Leaky" at an address that refuses connections, with the API key "leaky-api-key-0685"
    And the health checks have run
    When the daemon logs the warning "indexer test failed: http://127.0.0.1:1/api?t=caps&apikey=leaky-api-key-0685"
    And the daemon logs the warning "the tracker rejected the key leaky-api-key-0685"
    And the daemon logs the warning "qbittorrent at http://admin:hunter2-0685@qbit:8080 said no"
    And the client requests GET "/api/v1/log?pageSize=50"
    Then the response status is 200
    And the response body contains "apikey=***"
    And the response body contains "the tracker rejected the key ***"
    And the response body contains "http://admin:***@qbit:8080"
    And the response body does not contain "leaky-api-key-0685"
    And the response body does not contain "hunter2-0685"

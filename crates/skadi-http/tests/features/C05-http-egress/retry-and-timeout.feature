Feature: HTTP egress client — retries, backoff and timeouts (C05)
  One shared reqwest client for indexers, downloaders, metadata and notifiers.
  Idempotent GETs are retried with doubling backoff on transient failures
  (timeouts, connection errors, 408/429/5xx); every failure maps to
  AppError::Network. Mirrors Sonarr's HttpClient retry-on-5xx behaviour.

  @C05 @passing
  Scenario: a successful GET returns the body
    Given an egress client with the default retry policy
    And the upstream answers "/ok" with 200 and body "hello"
    When the client GETs "/ok"
    Then the request succeeds with body "hello"

  @C05 @passing
  Scenario Outline: transient upstream statuses are retried until the upstream recovers
    Given an egress client with a 1000 ms timeout, 3 retries and a 1 ms base backoff
    And the upstream answers "/flaky" with <status> for the first 2 calls, then 200 "recovered"
    When the client GETs "/flaky"
    Then the request succeeds with body "recovered"
    And the upstream saw exactly the expected number of calls

    Examples:
      | status |
      | 503    |
      | 500    |
      | 502    |
      | 429    |
      | 408    |

  @C05 @passing
  Scenario: the client gives up after the configured retries and reports the last status
    Given an egress client with a 1000 ms timeout, 3 retries and a 1 ms base backoff
    And the upstream always answers "/down" with 500
    When the client GETs "/down"
    Then the request fails with a network error
    And the error mentions "HTTP 500"

  @C05 @passing
  Scenario Outline: client errors are final and never retried
    Given an egress client with a 1000 ms timeout, 3 retries and a 1 ms base backoff
    And the upstream answers "/nope" with <status> for the first 1 calls, then 200 "never"
    When the client GETs "/nope"
    Then the request fails with a network error
    And the error mentions "HTTP <status>"

    Examples:
      | status |
      | 401    |
      | 403    |
      | 400    |

  @C05 @passing @SKADI-T-0502
  Scenario: a 404 is final too, but reported as not-found rather than a network error
    # The upstream is up and answering "gone". Reporting that as a network failure
    # made a deleted TMDB movie and a TMDB outage indistinguishable to the
    # metadata sync, which then retried a permanent absence forever.
    Given an egress client with a 1000 ms timeout, 3 retries and a 1 ms base backoff
    And the upstream answers "/nope" with 404 for the first 1 calls, then 200 "never"
    When the client GETs "/nope"
    Then the request fails with a not-found error
    And the error mentions "HTTP 404"

  @C05 @passing
  Scenario: the backoff doubles between attempts
    Given an egress client with a 1000 ms timeout, 3 retries and a 40 ms base backoff
    And the upstream always answers "/down" with 503
    When the client GETs "/down"
    Then the request fails with a network error
    And the request took at least 280 ms

  @C05 @passing
  Scenario: a slow upstream trips the per-attempt timeout instead of hanging
    Given an egress client with a 150 ms timeout, 0 retries and a 1 ms base backoff
    And the upstream answers "/slow" only after a 1500 ms delay
    When the client GETs "/slow"
    Then the request fails with a network error
    And the request took less than 1200 ms

  @C05 @passing
  Scenario: a timed-out attempt is retried like any other transient failure
    Given an egress client with a 100 ms timeout, 2 retries and a 1 ms base backoff
    And the upstream answers "/slow" only after a 1500 ms delay
    When the client GETs "/slow"
    Then the request fails with a network error
    And the request took at least 300 ms
    And the request took less than 2000 ms

  @C05 @passing
  Scenario: a refused connection is retried then surfaced as a network error
    Given an egress client with a 1000 ms timeout, 2 retries and a 1 ms base backoff
    When the client GETs the closed port "http://127.0.0.1:9/never"
    Then the request fails with a network error
    And the request took less than 3000 ms

  @C05 @passing
  Scenario: JSON and byte bodies decode through the same retrying path
    Given an egress client with the default retry policy
    And the upstream answers "/caps" with JSON:
      """
      { "limit": 100 }
      """
    And the upstream answers "/blob" with 200 and body "abcdef"
    When the client GETs "/caps" as JSON
    Then the request succeeds with JSON:
      """
      { "limit": 100 }
      """
    When the client GETs "/blob" as bytes
    Then the request succeeds with "6 bytes"

  @C05 @passing
  Scenario: a non-idempotent POST through the raw client is sent exactly once
    Given an egress client with a 1000 ms timeout, 3 retries and a 1 ms base backoff
    And the upstream answers POST "/login" with 503 and expects exactly 1 call
    When the client POSTs "/login" through the raw client
    Then the request succeeds with "HTTP 503 Service Unavailable"
    And the upstream saw exactly the expected number of calls

  @C05 @passing @SKADI-T-0510
  Scenario: a 429 with Retry-After is honoured before the next attempt
    Given an egress client with a 5000 ms timeout, 3 retries and a 1 ms base backoff
    And the upstream answers "/limited" with 429 and Retry-After 2 s for the first call, then 200
    When the client GETs "/limited"
    Then the request succeeds with body "ok"
    And the request took at least 2000 ms

  @C05 @passing @SKADI-T-0522
  Scenario: every request identifies itself with a skadi User-Agent by default
    Given an egress client with a 1000 ms timeout, 0 retries and a 1 ms base backoff
    And the upstream answers "/ua" only to a User-Agent containing "skadi"
    When the client GETs "/ua"
    Then the request succeeds with body "identified"

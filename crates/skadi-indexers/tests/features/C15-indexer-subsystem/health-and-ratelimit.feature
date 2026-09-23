Feature: Per-indexer health accounting and request rate limiting
  Every built indexer is wrapped in HealthTracked (records success/failure streaks
  into the process-global registry the hunter's circuit reads) and RateLimited (a
  token bucket per indexer). Sonarr equivalent: indexer status + escalating
  backoff ("disabled until"); Prowlarr: per-indexer request limits.

  @C15 @passing
  Scenario: failures build a consecutive streak with the last error, and a success resets it
    Given a health-tracked indexer that fails every call with "boom"
    And a "movie" query for "x"
    When the hunter searches the indexer 3 times
    Then the indexer health shows 3 consecutive failures
    And the indexer health records the last error "Network error: boom"
    And the indexer is reported unhealthy
    When one failure is forgiven
    Then the indexer health shows 2 consecutive failures

  @C15 @passing
  Scenario: rss and health checks count toward the same streak as searches
    Given a health-tracked indexer that fails every call with "down"
    When the hunter pulls the RSS feed
    And the indexer health check runs
    Then the indexer health shows 2 consecutive failures
    And the indexer health shows 0 successes and 2 failures in total

  @C15 @passing
  Scenario: a working indexer is healthy after a successful search and rss pull
    Given a health-tracked stub indexer
    And a "movie" query for "Sintel"
    When the hunter searches the indexer
    And the hunter pulls the RSS feed
    Then 0 releases are returned
    And the indexer health shows 2 successes and 0 failures in total
    And the indexer is reported healthy

  @C15 @passing
  Scenario: capability negotiation is not a health signal
    Given a health-tracked indexer that fails every call with "caps down"
    When the indexer capabilities are negotiated
    Then the capability negotiation fails
    And the indexer health has no entry yet

  @C15 @passing
  Scenario: a full bucket bursts then throttles at the refill rate
    Given a token bucket of capacity 3 refilling 1 per second
    When 4 requests are attempted at t=0s
    Then attempts 1 through 3 pass immediately
    And attempt 4 must wait about 1000 ms

  @C15 @passing
  Scenario: the bucket refills over time and never overfills past its capacity
    Given a token bucket of capacity 2 refilling 1 per second
    When 3 requests are attempted at t=0s
    And 2 requests are attempted at t=1s
    And 3 requests are attempted at t=101s
    Then attempts 1 through 2 pass immediately
    And attempt 3 must wait about 1000 ms
    And attempts 4 through 4 pass immediately
    And attempt 5 must wait about 1000 ms
    And attempts 6 through 7 pass immediately
    And attempt 8 must wait about 1000 ms

  @C15 @passing
  Scenario: a rate of 0 means unlimited and the decorator is skipped entirely
    Given a rate-limited stub indexer allowing 0 requests per minute
    And a "movie" query for "Sintel"
    When the hunter searches the indexer 20 times
    Then 1 release is returned

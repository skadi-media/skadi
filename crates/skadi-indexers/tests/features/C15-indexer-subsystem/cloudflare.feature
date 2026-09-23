Feature: Cloudflare-fronted trackers and the FlareSolverr-down path
  Prod 2026-09-06: with FlareSolverr down, a challenge page reached the worker as a
  ".torrent" ("add failed: error decoding torrent"). The contract: an HTML challenge
  body must never be enqueued as a torrent, and a blocked tracker must fail its
  search (Prowlarr: "CloudFlare protection detected, FlareSolverr not configured")
  rather than silently return nothing and count as healthy.

  @C15 @passing
  Scenario: a 503 "Just a moment" page is recognised as a challenge, a plain 403 is not
    Given a cardigann indexer from the "public_json" definition
    And the tracker answers "/blocked" with HTTP 503 and the fixture "challenge_html"
    And the tracker answers "/denied" with HTTP 403 and the fixture "not_xml"
    When the cardigann fetcher without the solver fetches "/blocked" from the tracker
    Then the fetched page is recognised as a challenge
    When the cardigann fetcher without the solver fetches "/denied" from the tracker
    Then the fetched page is not recognised as a challenge

  @C15 @passing
  Scenario: with FlareSolverr up, the challenge is solved and the search returns releases
    Given a FlareSolverr solver that answers with the fixture "solver_ok"
    And a cardigann indexer from the "public_json" definition using the solver
    And the tracker answers "/search" with HTTP 503 and the fixture "challenge_html"
    And a "movie" query for "The Matrix"
    When the hunter searches the indexer
    Then 1 release is returned
    And release 1's fetch is a magnet containing "btih:AAAABBBB"
    And the solver received 1 request to "/v1"

  @C15 @passing
  Scenario: with FlareSolverr down, the fetcher hands back the unsolved challenge page
    Given a FlareSolverr solver that answers with the fixture "solver_error"
    And a cardigann indexer from the "public_json" definition
    And the tracker answers "/blocked" with HTTP 503 and the fixture "challenge_html"
    When the cardigann fetcher with the solver fetches "/blocked" from the tracker
    Then the fetched page is still an unsolved challenge

  @C15 @passing @SKADI-T-0496
  Scenario: with FlareSolverr down, a challenged search fails instead of reporting an empty, healthy result
    Given a FlareSolverr solver that answers with the fixture "solver_error"
    And a cardigann indexer from the "public_json" definition using the solver
    And the tracker answers "/search" with HTTP 503 and the fixture "challenge_html"
    And a "movie" query for "The Matrix"
    When the hunter searches the indexer
    Then the search fails

  @C15 @passing @SKADI-T-0496
  Scenario: with no FlareSolverr configured, a challenged tracker fails its search and its health check
    Given a cardigann indexer from the "public_json" definition
    And the tracker answers "/search" with HTTP 503 and the fixture "challenge_html"
    And the tracker answers "/" with HTTP 503 and the fixture "challenge_html"
    And a "movie" query for "The Matrix"
    When the hunter searches the indexer
    Then the search fails
    When the indexer health check runs
    Then the health check fails

  @C15 @passing @SKADI-T-0497
  Scenario: a .torrent link on a challenge-fronted tracker is resolved through the indexer's own client at grab time, never handed to the worker bare
    Given a cardigann indexer from the "torrent_link" definition
    And the tracker answers "/search" with the fixture "tracker_torrent_link_rows"
    And the tracker answers "/dl/1.torrent" with HTTP 503 and the fixture "challenge_html"
    And a "movie" query for "Heat"
    When the hunter searches the indexer
    And the grab resolves release 1's fetch
    Then the grab does not hand the download client a bare .torrent URL

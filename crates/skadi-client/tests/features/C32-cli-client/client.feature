Feature: C32 typed client
  `skadi_client::Client` is the one HTTP client the CLI (and any third-party
  tool) uses: `/api/v1` prefix, bearer token on every request, JSON bodies out,
  and a structured `ApiError`. Yardstick: the *arr API clients (pyarr, the
  official SDKs) — typed errors, timeouts, and the server's error body surfaced.

  @C32 @passing
  Scenario: every call is rooted at /api/v1 and carries the bearer token
    Given a daemon that answers 200 with body "{\"status\":\"ok\",\"version\":\"0.0.1\"}"
    And a client configured with token "s3cret"
    When the client calls "health"
    Then the call succeeds
    And the daemon saw GET "/api/v1/health"
    And the daemon saw the header "authorization" with value "Bearer s3cret"
    And the call returns the JSON field "status" equal to "ok"

  @C32 @passing
  Scenario: an open-mode client sends no Authorization header
    Given a daemon that answers 200 with body "[]"
    And a client configured without a token
    When the client calls "domains"
    Then the daemon saw GET "/api/v1/domains"
    And the daemon saw no "authorization" header

  @C32 @passing
  Scenario: a trailing slash on the base URL does not double up
    Given a daemon that answers 200 with body "[]"
    And the daemon base URL has a trailing slash
    And a client configured without a token
    When the client calls "domains"
    Then the daemon saw GET "/api/v1/domains"

  @C32 @passing
  Scenario: domain toggles are PUT with a JSON body
    Given a daemon that answers 200 with body "{\"name\":\"movies\",\"enabled\":true}"
    And a client configured without a token
    When the client calls "enable movies"
    Then the daemon saw PUT "/api/v1/domains/movies"
    And the daemon saw the JSON body field "enabled" equal to "true"

  @C32 @passing
  Scenario: settings are created with the caller's document as the body
    Given a daemon that answers 201 with body "{\"id\":\"abc\"}"
    And a client configured without a token
    When the client calls "create_setting profiles" with body:
      """
      { "name": "HD", "cutoff": "Bluray-1080p" }
      """
    Then the daemon saw POST "/api/v1/settings/profiles"
    And the daemon saw the JSON body field "name" equal to "HD"
    And the call returns the JSON field "id" equal to "abc"

  @C32 @passing
  Scenario: optional filters become query parameters
    Given a daemon that answers 200 with body "[]"
    And a client configured without a token
    When the client calls "list_movies monitored"
    Then the daemon saw GET "/api/v1/movies?monitored=true"
    When the client calls "library movies monitored"
    Then the daemon saw GET "/api/v1/library?kind=movie&monitored=true"
    When the client calls "list_movies"
    Then the daemon saw GET "/api/v1/movies"

  @C32 @passing
  Scenario: a 204 delete yields JSON null rather than a decode error
    Given a daemon that answers 204 with body ""
    And a client configured without a token
    When the client calls "delete_setting profiles abc"
    Then the daemon saw DELETE "/api/v1/settings/profiles/abc"
    And the call returns JSON null

  @C32 @passing
  Scenario: a 401 is the Unauthorized error
    Given a daemon that answers 401 with body "{\"error\":\"unauthorized\",\"message\":\"missing or invalid bearer token\"}"
    And a client configured with token "wrong"
    When the client calls "domains"
    Then the call fails with Unauthorized

  @C32 @passing
  Scenario: a 404 is the NotFound error carrying the server's body
    Given a daemon that answers 404 with body "{\"error\":\"not_found\",\"message\":\"movie m1 not found\"}"
    And a client configured without a token
    When the client calls "get_movie m1"
    Then the call fails with NotFound
    And the error text contains "movie m1 not found"

  @C32 @passing
  Scenario: any other failure status is a Status error with the code and body
    Given a daemon that answers 500 with body "{\"error\":\"internal\",\"message\":\"boom\"}"
    And a client configured without a token
    When the client calls "activity"
    Then the call fails with Status
    And the error carries the status 500
    And the error text contains "boom"

  @C32 @passing
  Scenario: a successful reply that is not JSON is a Decode error
    Given a daemon that answers 200 with a non-JSON body
    And a client configured without a token
    When the client calls "health"
    Then the call fails with Decode

  @C32 @passing
  Scenario: a daemon that is not running is a Network error
    Given no daemon is listening
    And a client configured without a token
    When the client calls "health"
    Then the call fails with Network

  @C32 @passing
  Scenario: the provider test endpoint is POSTed and its ok flag surfaced verbatim
    Given a daemon that answers 200 with body "{\"ok\":false,\"error\":\"connection refused\"}"
    And a client configured without a token
    When the client calls "test_setting indexers abc"
    Then the daemon saw POST "/api/v1/settings/indexers/abc/test"
    And the call returns the JSON field "ok" equal to "false"

  @C32 @passing @SKADI-T-0470
  Scenario: the server's error envelope is decoded into a structured error kind
    Given a daemon that answers 400 with body "{\"error\":\"validation\",\"message\":\"cutoff must be one of the allowed qualities\"}"
    And a client configured without a token
    When the client calls "create_setting profiles" with body:
      """
      { "name": "bad" }
      """
    Then the call fails with Status
    And the error text contains "validation: cutoff must be one of the allowed qualities"

  @C32 @passing @SKADI-T-0470
  Scenario: a request has a timeout so a wedged daemon cannot hang the CLI forever
    Given a daemon that never answers
    And a client configured without a token
    When the client calls "health" and is given 3 seconds
    Then the call fails with Network
    And the client gave up on its own before the scenario deadline

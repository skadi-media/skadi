Feature: C30 error envelope
  Handler errors map to `{ "error": <kind>, "message": <text> }` with a stable
  status (skadi-api error.rs). Sonarr/Radarr return a JSON body for every
  error; here axum's built-in rejections and the outer 404 fall back to plain
  text, so clients cannot rely on the envelope.

  Background:
    Given a daemon running in open mode

  @C30 @passing
  Scenario: an unknown resource is 404 with the not_found kind
    When the client requests GET "/api/v1/settings/profiles/does-not-exist"
    Then the response status is 404
    And the response is a JSON error of kind "not_found"
    And the error message contains "profiles/does-not-exist"

  @C30 @passing
  Scenario: an unknown settings kind is 404 so the surface is exactly the five entities
    When the client requests GET "/api/v1/settings/root_folders"
    Then the response status is 404
    And the response is a JSON error of kind "not_found"

  @C30 @passing
  Scenario: a validation failure is 400 with the validation kind
    When the client sends POST "/api/v1/settings/profiles" with body:
      """
      { "name": "bad", "allowed": ["No-Such-Quality"] }
      """
    Then the response status is 400
    And the response is a JSON error of kind "validation"
    And the error message contains "unknown quality"

  @C30 @passing
  Scenario: an empty release key on a manual block is a validation error
    When the client sends POST "/api/v1/blocklist" with body:
      """
      { "release_key": "  ", "title": "x" }
      """
    Then the response status is 400
    And the response is a JSON error of kind "validation"

  @C30 @passing
  Scenario: an unknown domain name is 404 with the envelope
    When the client sends PUT "/api/v1/domains/podcasts" with body:
      """
      { "enabled": true }
      """
    Then the response status is 404
    And the response is a JSON error of kind "not_found"

  @C30 @passing @SKADI-T-0458 @SKADI-T-0458
  Scenario: an unknown API path is a 404 with the JSON envelope, not plain text
    When the client requests GET "/api/v1/no-such-route"
    Then the response status is 404
    And the response is a JSON error of kind "not_found"

  @C30 @passing @SKADI-T-0458 @SKADI-T-0458
  Scenario: a malformed JSON body is rejected with the JSON envelope
    When the client sends PUT "/api/v1/domains/movies" with the raw body "{ not json"
    Then the response status is 400
    And the response is a JSON error of kind "validation"

  @C30 @passing @SKADI-T-0458 @SKADI-T-0458
  Scenario: a wrongly-typed field is rejected with the JSON envelope
    Given the daemon compiles in the "movies" domain
    When the client sends PUT "/api/v1/domains/movies" with body:
      """
      { "enabled": "yes" }
      """
    Then the response is a JSON error of kind "validation"

  @C30 @passing @SKADI-T-0458 @SKADI-T-0458
  Scenario: a missing required query parameter is rejected with the JSON envelope
    When the client requests GET "/api/v1/search"
    Then the response status is 400
    And the response is a JSON error of kind "validation"

  @C30 @gap
  Scenario: a wrong method is 405 with the JSON envelope
    When the client requests DELETE "/api/v1/domains"
    Then the response status is 405
    And the response is a JSON error of kind "method_not_allowed"

Feature: C33 bearer-token authentication
  Every /api/v1 route except the liveness probe requires
  `Authorization: Bearer <SKADI_API_TOKEN>`; without a configured token the API
  runs in open mode. Sonarr/Radarr's yardstick is API-key auth (X-Api-Key header
  or ?apikey= query) with a proper 401 challenge.

  @C33 @passing
  Scenario: the liveness probe is reachable without a token
    Given a daemon protected by API token "s3cret"
    And the client presents no token
    When the client requests GET "/api/v1/health"
    Then the response status is 200
    And the response field "status" is "ok"

  @C33 @passing
  Scenario: a protected route without a token is 401 with the error envelope
    Given a daemon protected by API token "s3cret"
    And the client presents no token
    When the client requests GET "/api/v1/domains"
    Then the response status is 401
    And the response is a JSON error of kind "unauthorized"

  @C33 @passing
  Scenario: a wrong token is rejected
    Given a daemon protected by API token "s3cret"
    And the client presents token "nope"
    When the client requests GET "/api/v1/domains"
    Then the response status is 401
    And the response is a JSON error of kind "unauthorized"

  @C33 @passing
  Scenario: a token that is a prefix of the real one is rejected
    Given a daemon protected by API token "s3cret"
    And the client presents token "s3cre"
    When the client requests GET "/api/v1/domains"
    Then the response status is 401

  @C33 @passing
  Scenario: the right token is accepted
    Given a daemon protected by API token "s3cret"
    And the client presents token "s3cret"
    When the client requests GET "/api/v1/domains"
    Then the response status is 200

  @C33 @passing
  Scenario: open mode lets every request through
    Given a daemon running in open mode
    And the client presents no token
    When the client requests GET "/api/v1/settings/profiles"
    Then the response status is 200

  @C33 @passing
  Scenario: there is a single role — any authenticated caller may clear the whole blocklist
    Given a daemon protected by API token "s3cret"
    And the client presents token "s3cret"
    # `?all=true` since SKADI-T-0473: the point here is that authentication alone
    # authorises the wipe (no roles), not that it can be reached by omission.
    When the client requests DELETE "/api/v1/blocklist?all=true"
    Then the response status is 200
    And the response field "removed" is "0"

  @C33 @passing @SKADI-T-0466
  Scenario: a 401 carries a WWW-Authenticate challenge (RFC 7235)
    Given a daemon protected by API token "s3cret"
    And the client presents no token
    When the client requests GET "/api/v1/domains"
    Then the response status is 401
    And the response header "www-authenticate" starts with "Bearer"

  @C33 @passing @SKADI-T-0466
  Scenario: the Bearer scheme is matched case-insensitively (RFC 6750)
    Given a daemon protected by API token "s3cret"
    And the client presents the authorization header "bearer s3cret"
    When the client requests GET "/api/v1/domains"
    Then the response status is 200

  @C33 @passing @SKADI-T-0466
  Scenario: Sonarr-style X-Api-Key header authentication
    Given a daemon protected by API token "s3cret"
    And the client presents no token
    And the client presents the header "x-api-key" with value "s3cret"
    When the client requests GET "/api/v1/domains"
    Then the response status is 200

  @C33 @passing @SKADI-T-0466
  Scenario: Sonarr-style apikey query authentication for feed URLs
    Given a daemon protected by API token "s3cret"
    And the client presents no token
    When the client requests GET "/api/v1/domains?apikey=s3cret"
    Then the response status is 200

  @C33 @passing @SKADI-T-0466
  Scenario: rotating the api_token in the config table takes effect without a restart
    Given a daemon protected by API token "old-token"
    When the config key "api_token" is set to "new-token"
    And the daemon re-reads its settings
    And the client presents token "new-token"
    And the client requests GET "/api/v1/domains"
    Then the response status is 200

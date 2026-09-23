Feature: C30 settings entities
  `/settings/{kind}` is the generic config store for indexers, downloaders,
  profiles, notifiers and custom_formats (Radarr: /indexer, /downloadclient,
  /qualityprofile, /notification, /customformat). Secrets are sealed in the
  credential store and never echoed; profiles and custom formats are validated
  and normalised at write time.

  Background:
    Given a daemon running in open mode

  @C30 @passing
  Scenario: full lifecycle of a quality profile
    When the client requests GET "/api/v1/settings/profiles"
    Then the response body is a JSON array of length 0
    When the client sends POST "/api/v1/settings/profiles" with body:
      """
      { "name": "HD", "allowed": ["Bluray-720p", "Bluray-1080p"], "cutoff": "Bluray-1080p" }
      """
    Then the response status is 201
    And the created id is remembered as "hd"
    And the response field "body.name" is "HD"
    And the response field "body.media_type" is "video"
    And the response field "body.upgrade_allowed" is "true"
    And the response field "has_secret" is absent
    When the client requests GET "/api/v1/settings/profiles/<hd>"
    Then the response status is 200
    And the response field "id" is "<hd>"
    When the client sends PUT "/api/v1/settings/profiles/<hd>" with body:
      """
      { "name": "HD renamed", "allowed": ["Bluray-1080p"], "cutoff": "Bluray-1080p" }
      """
    Then the response status is 200
    And the response field "body.name" is "HD renamed"
    When the client requests GET "/api/v1/settings/profiles"
    Then the response body is a JSON array of length 1
    When the client requests DELETE "/api/v1/settings/profiles/<hd>"
    Then the response status is 204
    When the client requests DELETE "/api/v1/settings/profiles/<hd>"
    Then the response status is 404

  @C30 @passing
  Scenario: quality names are resolved to canonical ids and a cutoff outside allowed is refused
    When the client sends POST "/api/v1/settings/profiles" with body:
      """
      { "name": "x", "allowed": ["Bluray-1080p"], "cutoff": "Bluray-2160p" }
      """
    Then the response status is 400
    # SKADI-T-0525: the field is now structured rather than prefixed into the
    # message, so a form can highlight the right input.
    And the error field is "cutoff"
    And the error message contains "must be one of the allowed"

  @C30 @passing
  Scenario: profiles filter by media type; unstamped rows are video
    Given a stored profiles setting remembered as "v" with body:
      """
      { "name": "video-one" }
      """
    When the client requests GET "/api/v1/settings/profiles?media_type=video"
    Then the response body is a JSON array of length 1
    When the client requests GET "/api/v1/settings/profiles?media_type=audio"
    Then the response body is a JSON array of length 0

  @C30 @passing
  Scenario: an indexer's api key is sealed, never echoed, and cascades on delete
    When the client sends POST "/api/v1/settings/indexers" with body:
      """
      { "kind": "torznab", "name": "nzbgeek", "base_url": "http://127.0.0.1:1", "categories": [2000], "api_key": "abc" }
      """
    Then the response status is 201
    And the created id is remembered as "ixr"
    And the response field "has_secret" is "true"
    And the response field "body.api_key" is absent
    When the client requests GET "/api/v1/settings/indexers/<ixr>"
    Then the response field "body.api_key" is absent
    And the response field "has_secret" is "true"
    When the client sends PUT "/api/v1/settings/indexers/<ixr>" with body:
      """
      { "kind": "torznab", "name": "renamed", "base_url": "http://127.0.0.1:1", "categories": [2000] }
      """
    Then the response status is 200
    And the response field "has_secret" is "true"
    When the client sends PUT "/api/v1/settings/indexers/<ixr>" with body:
      """
      { "kind": "torznab", "name": "renamed", "base_url": "http://127.0.0.1:1", "categories": [2000], "api_key": "" }
      """
    Then the response status is 400
    And the response is a JSON error of kind "validation"

  @C30 @passing
  Scenario: a downloader created without a password reports no secret
    When the client sends POST "/api/v1/settings/downloaders" with body:
      """
      { "kind": "skadi", "name": "built-in" }
      """
    Then the response status is 201
    And the response field "has_secret" is "false"

  @C30 @passing
  Scenario: custom formats require a domain, a name and at least one compilable rule
    When the client sends POST "/api/v1/settings/custom_formats" with body:
      """
      { "domain": "movie", "name": "x265", "rules": [{ "Codec": "x265" }] }
      """
    Then the response status is 201
    And the response field "body.domain" is "Movie"
    When the client sends POST "/api/v1/settings/custom_formats" with body:
      """
      { "domain": "movie", "name": "bad", "rules": [{ "TitleRegex": "(" }] }
      """
    Then the response status is 400
    And the response is a JSON error of kind "validation"
    When the client sends POST "/api/v1/settings/custom_formats" with body:
      """
      { "domain": "series", "name": "x", "rules": [{ "TitleRegex": "x" }] }
      """
    Then the response status is 400

  @C30 @passing
  Scenario: PUT on a missing id is 404 rather than an upsert
    When the client sends PUT "/api/v1/settings/notifiers/nope" with body:
      """
      { "kind": "webhook", "name": "n" }
      """
    Then the response status is 404
    And the response is a JSON error of kind "not_found"

  @C30 @passing
  Scenario: testing a provider that cannot be built is a request error, a failed test is 200 ok:false
    When the client requests POST "/api/v1/settings/indexers/nope/test"
    Then the response status is 404
    Given a stored indexers setting remembered as "dead" with body:
      """
      { "kind": "torznab", "name": "dead", "base_url": "http://127.0.0.1:1", "categories": [2000], "api_key": "k" }
      """
    When the client requests POST "/api/v1/settings/indexers/<dead>/test"
    Then the response status is 200
    And the response field "ok" is "false"
    And the response field "error" is present

  @C30 @passing @SKADI-T-0471
  Scenario: partial updates with PATCH keep the untouched fields
    Given a stored profiles setting remembered as "p" with body:
      """
      { "name": "keep-me", "allowed": ["Bluray-1080p"], "cutoff": "Bluray-1080p" }
      """
    When the client sends PATCH "/api/v1/settings/profiles/<p>" with body:
      """
      { "upgrade_allowed": false }
      """
    Then the response status is 200
    And the response field "body.name" is "keep-me"
    And the response field "body.upgrade_allowed" is "false"

  @C30 @passing @SKADI-T-0471
  Scenario: settings documents are validated against a per-kind schema on write
    When the client sends POST "/api/v1/settings/indexers" with body:
      """
      { "kind": "torznab", "name": "no-url" }
      """
    Then the response status is 400
    And the response is a JSON error of kind "validation"

  @C30 @passing @SKADI-T-0532
  Scenario: exactly one profile per media type is the default, and marking a new one clears the old
    When the client sends POST "/api/v1/settings/profiles" with body:
      """
      { "name": "first", "default": true }
      """
    Then the response status is 201
    And the created id is remembered as "a"
    When the client sends POST "/api/v1/settings/profiles" with body:
      """
      { "name": "second", "default": true }
      """
    Then the response status is 201
    And the created id is remembered as "b"
    # Marking "second" default must have cleared "first" — two defaults would put
    # the fallback back to whichever row sorts first, the arbitrariness this fixes.
    When the client requests GET "/api/v1/settings/profiles/<a>"
    Then the response field "body.default" is "false"
    When the client requests GET "/api/v1/settings/profiles/<b>"
    Then the response field "body.default" is "true"

  @C30 @passing @SKADI-T-0532
  Scenario: the default profile cannot be deleted out from under the fallback
    When the client sends POST "/api/v1/settings/profiles" with body:
      """
      { "name": "the default", "default": true }
      """
    Then the response status is 201
    And the created id is remembered as "d"
    When the client requests DELETE "/api/v1/settings/profiles/<d>"
    Then the response status is 400
    And the error message contains "mark another profile default"
    # Still there: a refused delete must not half-happen.
    When the client requests GET "/api/v1/settings/profiles/<d>"
    Then the response status is 200

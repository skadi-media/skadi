Feature: C30 route families on the base router
  Happy paths for the control-plane routes that live in skadi-api itself:
  domains, blocklist, quality definitions, naming, download settings, search,
  activity and the folder browser. Domain routes (movies/series/books) are
  covered by their own crates.

  Background:
    Given a daemon running in open mode

  @C30 @passing
  Scenario: compiled-in domains are listed disabled by default and can be toggled
    Given the daemon compiles in the "movies" domain
    And the daemon compiles in the "television" domain
    When the client requests GET "/api/v1/domains"
    Then the response body is a JSON array of length 2
    And the response field "0.name" is "movies"
    And the response field "0.kind" is "movie"
    And the response field "0.enabled" is "false"
    And the response field "1.kind" is "series"
    When the client sends PUT "/api/v1/domains/television" with body:
      """
      { "enabled": true }
      """
    Then the response status is 200
    And the response field "enabled" is "true"
    When the client requests GET "/api/v1/domains"
    Then the response field "1.enabled" is "true"

  @C30 @passing
  Scenario: manual blocklist entries are created, listed, scoped and removed
    When the client sends POST "/api/v1/blocklist" with body:
      """
      { "release_key": "abc", "title": "Bad.Release", "acquirable_ref": "movie:1" }
      """
    Then the response status is 201
    And the created id is remembered as "blk"
    And the response field "reason" is "manual"
    And the response field "expires_at" is "null"
    When the client sends POST "/api/v1/blocklist" with body:
      """
      { "release_key": "def", "title": "Temp.Release", "ttl_seconds": 3600 }
      """
    Then the response field "expires_at" is present
    When the client requests GET "/api/v1/blocklist"
    Then the response body is a JSON array of length 2
    When the client requests GET "/api/v1/blocklist?acquirable=movie:1"
    Then the response body is a JSON array of length 1
    And the response field "0.release_key" is "abc"
    When the client requests DELETE "/api/v1/blocklist/<blk>"
    Then the response status is 204
    When the client requests DELETE "/api/v1/blocklist/<blk>"
    Then the response status is 404
    When the client requests DELETE "/api/v1/blocklist?all=true"
    Then the response field "removed" is "1"

  @C30 @passing
  Scenario: bulk unblock by ids leaves the rest in place
    When the client sends POST "/api/v1/blocklist" with body:
      """
      { "release_key": "a", "title": "A" }
      """
    And the created id is remembered as "a"
    And the client sends POST "/api/v1/blocklist" with body:
      """
      { "release_key": "b", "title": "B" }
      """
    And the client requests DELETE "/api/v1/blocklist?ids=<a>,missing"
    Then the response field "removed" is "1"
    When the client requests GET "/api/v1/blocklist"
    Then the response body is a JSON array of length 1
    And the response field "0.release_key" is "b"

  @C30 @passing
  Scenario: the built-in quality definitions are served low to high with stable ids
    When the client requests GET "/api/v1/quality/definitions"
    Then the response status is 200
    And the response field "0.rank" is "0"
    And the response field "0.id" is present
    And the response body contains "Bluray-1080p"

  @C30 @passing
  Scenario: naming templates default, persist and preview
    When the client requests GET "/api/v1/naming/settings"
    Then the response field "movie_file" is "{TitleKebab} ({Year})"
    And the response field "space" is "_"
    When the client sends PUT "/api/v1/naming/settings" with body:
      """
      { "movie_folder": "{TitleKebab} ({Year})", "movie_file": "{TitleKebab}", "series_folder": "", "series_file": "", "audiobook_folder": "", "audiobook_file": "", "space": "-" }
      """
    Then the response status is 204
    When the client requests GET "/api/v1/naming/settings"
    Then the response field "movie_file" is "{TitleKebab}"
    And the response field "space" is "-"
    And the response field "series_file" is "{SeriesTitleKebab} - {Episode}{EpisodeTitlePart}"
    When the client sends POST "/api/v1/naming/preview" with body:
      """
      { "domain": "movie", "folder": "{TitleKebab} ({Year})", "file": "{TitleKebab}", "space": "_" }
      """
    Then the response field "path" contains "big-buck-bunny"

  @C30 @passing
  Scenario: download worker settings default to unlimited and clamp the seed action
    When the client requests GET "/api/v1/downloads/settings"
    Then the response field "max_active" is "0"
    And the response field "seed_action" is "stop"
    When the client sends PUT "/api/v1/downloads/settings" with body:
      """
      { "max_active": 3, "down_limit_bps": 1000, "up_limit_bps": 0, "seed_ratio": 1.5, "seed_time_mins": 60, "seed_action": "banana" }
      """
    Then the response status is 204
    When the client requests GET "/api/v1/downloads/settings"
    Then the response field "max_active" is "3"
    And the response field "seed_ratio" is "1.5"
    And the response field "seed_action" is "stop"

  @C30 @passing
  Scenario: manual search with no indexers configured is an empty result, not an error
    When the client requests GET "/api/v1/search?q=matrix"
    Then the response status is 200
    And the response body is a JSON array of length 0
    When the client requests GET "/api/v1/search?q=%20"
    Then the response status is 400
    And the response is a JSON error of kind "validation"

  @C30 @passing
  Scenario: search-all is accepted immediately and the idle activity feed is empty
    When the client requests POST "/api/v1/search-all"
    Then the response status is 202
    And the response field "status" is "sweep requested"
    When the client requests GET "/api/v1/activity"
    Then the response status is 200
    When the client requests GET "/api/v1/downloads"
    Then the response body is a JSON array of length 0

  @C30 @passing
  Scenario: indexer health and categories are readable
    When the client requests GET "/api/v1/indexers/health"
    Then the response status is 200
    When the client requests GET "/api/v1/indexers/categories"
    Then the response status is 200
    And the response body contains "Movies"

  @C30 @passing @serial
  Scenario: the folder browser is confined to the configured roots
    Given the library root contains the folders "movies,tv,.hidden"
    And the environment variable "SKADI_BROWSE_ROOTS" is "<tmp>/library"
    When the client requests GET "/api/v1/fs/roots"
    Then the response body is a JSON array of length 1
    When the client requests GET "/api/v1/fs/browse"
    Then the response status is 200
    And the response field "entries.0.name" is "movies"
    And the response field "entries.1.name" is "tv"
    And the response field "parent" is "null"
    When the client requests GET "/api/v1/fs/browse?path=/etc"
    Then the response status is 400
    And the response is a JSON error of kind "validation"
    Given the environment variable "SKADI_BROWSE_ROOTS" is unset

  @C30 @passing @SKADI-T-0460 @serial
  Scenario: browsing with no existing roots is a client-visible condition, not a 500
    Given the environment variable "SKADI_BROWSE_ROOTS" is "/no/such/skadi/root"
    When the client requests GET "/api/v1/fs/browse"
    Then the response status is 200
    And the response field "entries" is "[]"
    Given the environment variable "SKADI_BROWSE_ROOTS" is unset

  @C30 @passing @SKADI-T-0472
  Scenario: the API publishes an OpenAPI description of itself
    When the client requests GET "/api/v1/openapi.json"
    Then the response status is 200
    And the response field "openapi" is present
    And the response field "paths" is present

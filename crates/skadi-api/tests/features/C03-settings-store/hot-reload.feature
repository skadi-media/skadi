Feature: Settings store — boot seeding order and the hot-reload contract (C03)
  Env is the boot-time authority (seeded into the config table, overwriting
  runtime values); between boots the table is the truth. Table-backed process
  fields (bind_addr, api_token) are resolved once after seeding; hot consumers
  re-read the table on their own cadence via the supervisor fingerprint.

  Background:
    Given a daemon running in open mode

  @C03 @passing @serial
  Scenario: at boot an env value overwrites a runtime edit while untouched runtime keys survive
    Given the config key "naming.space" is set to "-"
    And the config key "min_seeders" is set to "3"
    And the process environment sets "SKADI_NAMING_SPACE" to "_"
    When the daemon bootstraps with the "probe" domain compiled in
    Then the config key "naming.space" equals "_"
    And the config key "min_seeders" equals "3"

  @C03 @passing
  Scenario: the bind address and API token are resolved from the config table after seeding
    Given the config key "bind_addr" is set to "0.0.0.0:9090"
    And the config key "api_token" is set to "table-token"
    When the daemon resolves its runtime config from the table
    Then the daemon binds "0.0.0.0:9090" and requires the token "table-token"

  @C03 @passing
  Scenario: an empty API token in the table means open mode
    Given the config key "api_token" is set to ""
    When the daemon resolves its runtime config from the table
    Then the daemon binds "127.0.0.1:8080" and requires the token "none"

  @C03 @bug @SKADI-T-0459
  Scenario: saving download settings that change nothing does not rebuild the provider set
    Given 1 provider reloaders are registered with the supervisor
    When the supervisor reconciles
    And the client sends PUT "/api/v1/downloads/settings" with body:
      """
      { "max_active": 2, "down_limit_bps": 0, "up_limit_bps": 100000, "seed_ratio": 0.0, "seed_time_mins": 0, "seed_action": "stop" }
      """
    Then the response status is 204
    When the supervisor reconciles
    Then every reloader has been applied 2 times
    When the client sends PUT "/api/v1/downloads/settings" with body:
      """
      { "max_active": 2, "down_limit_bps": 0, "up_limit_bps": 100000, "seed_ratio": 0.0, "seed_time_mins": 0, "seed_action": "stop" }
      """
    And the supervisor reconciles
    Then every reloader has been applied 2 times

  @C03 @passing @SKADI-T-0524
  Scenario: a naming template can be reset to the built-in default by saving it blank
    When the client sends PUT "/api/v1/naming/settings" with body:
      """
      { "movie_folder": "{Title}", "movie_file": "", "series_folder": "", "series_file": "", "audiobook_folder": "", "audiobook_file": "", "space": "_" }
      """
    Then the response status is 204
    When the client sends PUT "/api/v1/naming/settings" with body:
      """
      { "movie_folder": "", "movie_file": "", "series_folder": "", "series_file": "", "audiobook_folder": "", "audiobook_file": "", "space": "_" }
      """
    Then the response status is 204
    When the client requests GET "/api/v1/naming/settings"
    # The scenario's own title is the contract: blank *resets*, so the read-back
    # is the built-in default, not the empty string it was saved as.
    Then the response field "movie_folder" is "{TitleKebab} ({Year}) {TmdbTag} {ImdbTag}/{EditionKebab}"

  @C03 @passing @SKADI-T-0535
  Scenario: the config plane is readable and writable over the API
    When the client requests GET "/api/v1/config/import.min_free_mb"
    Then the response status is 200
    And the response field "value" is "0"
    And the response field "editable" is "true"
    When the client sends PUT "/api/v1/config/import.min_free_mb" with body:
      """
      { "value": "512" }
      """
    Then the response status is 200
    And the response field "value" is "512"
    And the response field "source" is "runtime"
    # A reset drops the row so the registry default applies again — distinct from
    # setting the default explicitly, which would not be re-seeded from env.
    When the client requests DELETE "/api/v1/config/import.min_free_mb"
    Then the response status is 200
    And the response field "value" is "0"
    And the response field "isSet" is "false"

  @C03 @passing @SKADI-T-0535
  Scenario: a write is validated against the key's declared type before it is stored
    When the client sends PUT "/api/v1/config/worker.port_lo" with body:
      """
      { "value": "not-a-number" }
      """
    Then the response status is 400
    And the error field is "worker.port_lo"
    When the client requests GET "/api/v1/config/worker.port_lo"
    Then the response field "isSet" is "false"

  @C03 @passing @SKADI-T-0535
  Scenario: a cross-key constraint is checked against the config the write would produce
    # Defaults are lo=16881, hi=16891. Widening the top of the range is fine …
    When the client sends PUT "/api/v1/config/worker.port_hi" with body:
      """
      { "value": "17000" }
      """
    Then the response status is 200
    # … but a low end above it is refused, even though 18000 is a perfectly valid
    # u16 on its own. Only the pair is wrong, which is what per-key validation
    # cannot see — and unchecked it leaves the worker unable to bind.
    When the client sends PUT "/api/v1/config/worker.port_lo" with body:
      """
      { "value": "18000" }
      """
    Then the response status is 400
    And the error message contains "port range"
    When the client requests GET "/api/v1/config/worker.port_lo"
    Then the response field "isSet" is "false"

  @C03 @passing @SKADI-T-0535
  Scenario: the API token and the vault key are never echoed back
    When the client requests GET "/api/v1/config/api_token"
    Then the response status is 200
    And the response field "redacted" is "true"
    And the response has no field "value"

  @C03 @passing @SKADI-T-0535
  Scenario: a key read before the config table exists cannot be set at runtime
    When the client sends PUT "/api/v1/config/database_url" with body:
      """
      { "value": "sqlite://elsewhere.db" }
      """
    Then the response status is 400
    And the error field is "database_url"

  @C03 @passing @SKADI-T-0535
  Scenario: an unknown config key is a 404, not a silently-stored row
    When the client sends PUT "/api/v1/config/nope.not.a.key" with body:
      """
      { "value": "x" }
      """
    Then the response status is 404

Feature: C32 command-line client
  `skadi <subcommand>` is a thin HTTP client over a running daemon (only `run`
  is the daemon itself). Output is the API's JSON verbatim; failures go to
  stderr with a non-zero exit. Yardstick: an *arr CLI would cover every
  resource family (series, history, blocklist, wanted, queue, health).

  @C32 @passing
  Scenario: version needs no daemon
    When the operator runs "skadi version" against a daemon that is not running
    Then the command succeeds
    And stdout contains "skadi 0."

  @C32 @passing
  Scenario: health prints the daemon's liveness JSON
    Given a running daemon in open mode
    When the operator runs "skadi health"
    Then the command succeeds
    And stdout is JSON with field "status" equal to "ok"

  @C32 @passing
  Scenario: domains can be listed, enabled and disabled
    Given a running daemon in open mode
    When the operator runs "skadi domain list"
    Then the command succeeds
    And stdout is a JSON array of length 1
    And stdout is JSON with field "0.name" equal to "movies"
    And stdout is JSON with field "0.enabled" equal to "false"
    When the operator runs "skadi domain enable movies"
    Then the command succeeds
    And stdout is JSON with field "enabled" equal to "true"
    When the operator runs "skadi domain disable movies"
    Then stdout is JSON with field "enabled" equal to "false"

  @C32 @passing
  Scenario: settings are added from inline JSON, listed and removed
    Given a running daemon in open mode
    When the operator runs "skadi settings add profiles '{\"name\":\"HD\",\"cutoff\":\"Bluray-1080p\"}'"
    Then the command succeeds
    And stdout is JSON with field "body.name" equal to "HD"
    And the id printed is remembered as "hd"
    When the operator runs "skadi settings list profiles"
    Then stdout is a JSON array of length 1
    When the operator runs "skadi settings rm profiles <hd>"
    Then the command succeeds
    When the operator runs "skadi settings list profiles"
    Then stdout is a JSON array of length 0

  @C32 @passing
  Scenario: a malformed inline JSON body is refused before any request is made
    Given a running daemon in open mode
    When the operator runs "skadi settings add profiles '{not json'"
    Then the command exits 1
    And stderr contains "body must be valid JSON"

  @C32 @passing
  Scenario: a validation error from the daemon is reported on stderr with exit 1
    Given a running daemon in open mode
    When the operator runs "skadi settings add profiles '{\"name\":\"x\",\"allowed\":[\"Nope\"]}'"
    Then the command exits 1
    # SKADI-T-0470: the client decodes the daemon's envelope, so stderr names the
    # error kind and the reason instead of a bare status code.
    And stderr contains "validation:"
    And stderr contains "unknown quality in allowed"
    And stderr contains "unknown quality"

  @C32 @passing
  Scenario: a failed provider test is a non-zero exit, not just ok:false
    Given a running daemon in open mode
    When the operator runs "skadi settings add indexers '{\"kind\":\"torznab\",\"name\":\"dead\",\"base_url\":\"http://127.0.0.1:1\",\"categories\":[2000],\"api_key\":\"k\"}'"
    Then the command succeeds
    And the id printed is remembered as "dead"
    When the operator runs "skadi settings test indexers <dead>"
    Then the command exits 1
    And stdout is JSON with field "ok" equal to "false"
    When the operator runs "skadi settings test indexers nope"
    Then the command exits 1
    And stderr contains "not found"

  @C32 @passing
  Scenario: the token comes from the flag or SKADI_API_TOKEN, and a wrong one is unauthorized
    Given a running daemon protected by API token "s3cret"
    When the operator runs "skadi domain list"
    Then the command exits 1
    And stderr contains "unauthorized"
    When the operator runs "skadi --token s3cret domain list"
    Then the command succeeds
    When the operator runs "skadi domain list" with SKADI_API_TOKEN "s3cret"
    Then the command succeeds
    When the operator runs "skadi health"
    Then the command succeeds

  @C32 @passing
  Scenario: an unreachable daemon is a network error with exit 1
    When the operator runs "skadi health" against a daemon that is not running
    Then the command exits 1
    And stderr contains "network error"

  @C32 @passing
  Scenario: --pretty renders indented JSON
    Given a running daemon in open mode
    When the operator runs "skadi --pretty health"
    Then the command succeeds
    And stdout spans more than one line

  @C32 @passing
  Scenario: library and activity read through the unified views
    Given a running daemon in open mode
    When the operator runs "skadi library --kind movie --monitored true"
    Then the command succeeds
    And stdout is a JSON array of length 0
    When the operator runs "skadi activity"
    Then the command succeeds

  @C32 @passing
  Scenario: an unknown subcommand is a usage error
    Given a running daemon in open mode
    When the operator runs "skadi frobnicate"
    Then the command exits 2
    And stderr contains "unrecognized subcommand"

  @C32 @passing @SKADI-T-0469
  Scenario: television has CLI commands like movies and audiobooks do
    # This harness serves the base router only — mounting a domain router needs a
    # Cloacina runner, which is disproportionate for a CLI test and is covered by
    # skadi-tv's own suites. What is asserted here is the CLI's actual job: that
    # `series list` is a real command that builds and sends the request. An
    # unknown command would exit 2 from clap without ever reaching the daemon;
    # this reaches it and relays the daemon's own envelope.
    Given a running daemon in open mode
    When the operator runs "skadi series list"
    Then stderr contains "no such route"
    When the operator runs "skadi series --help"
    Then the command succeeds
    And stdout contains "List series"

  @C32 @passing @SKADI-T-0469
  Scenario: acquisition history is readable from the CLI
    Given a running daemon in open mode
    When the operator runs "skadi history --limit 10"
    Then the command succeeds

  @C32 @passing @SKADI-T-0469
  Scenario: the blocklist is manageable from the CLI
    Given a running daemon in open mode
    When the operator runs "skadi blocklist list"
    Then the command succeeds

  @C32 @passing @SKADI-T-0469
  Scenario: the wanted backlog and a search-all trigger exist in the CLI
    Given a running daemon in open mode
    When the operator runs "skadi wanted"
    Then the command succeeds
    When the operator runs "skadi search-all"
    Then the command succeeds

  @C32 @passing @SKADI-T-0469
  Scenario: an existing setting can be updated from the CLI
    Given a running daemon in open mode
    When the operator runs "skadi settings add profiles '{\"name\":\"HD\",\"cutoff\":\"Bluray-1080p\"}'"
    Then the command succeeds
    And the id printed is remembered as "hd"
    When the operator runs "skadi settings update profiles <hd> '{\"name\":\"HD2\",\"cutoff\":\"Bluray-1080p\"}'"
    Then the command succeeds
    When the operator runs "skadi settings list profiles"
    Then stdout is a JSON array of length 1
    And stdout contains "HD2"

  @C32 @passing @SKADI-T-0469
  Scenario: the health checks (not just liveness) are readable from the CLI
    Given a running daemon in open mode
    When the operator runs "skadi health --checks"
    Then the command succeeds

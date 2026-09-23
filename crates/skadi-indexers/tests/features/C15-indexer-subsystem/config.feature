Feature: Indexer settings rows (kind-tagged IndexerConfig) and the provider factory
  crates/skadi-indexers/src/config.rs is the stored shape for `kind = "indexers"`
  settings; secrets live in the credential store. Sonarr/Radarr equivalent: the
  indexer settings form (URL, API key, categories, enable RSS / automatic /
  interactive search, priority, minimum seeders, seed ratio/time).

  @C15 @passing
  Scenario: a torznab row builds a health-tracked, rate-limited indexer
    Given the indexer settings row:
      """
      { "kind": "torznab", "name": "geek", "base_url": "https://api.nzbgeek.info", "categories": [2000, 2040] }
      """
    When the provider factory builds the indexer
    Then the indexer builds
    And the settings row round-trips with name "geek"
    And the settings row is not a built-in indexer

  @C15 @passing
  Scenario: a torznab row without an http(s) base URL is rejected
    Given the indexer settings row:
      """
      { "kind": "torznab", "name": "bad", "base_url": "not-a-url", "categories": [2000] }
      """
    When the provider factory builds the indexer
    Then the build is rejected with a validation error mentioning "base_url"

  @C15 @passing
  Scenario: a prowlarr row without categories is rejected
    Given the indexer settings row:
      """
      { "kind": "prowlarr", "name": "pw", "base_url": "http://gluetun:9696", "categories": [] }
      """
    When the provider factory builds the indexer
    Then the build is rejected with a validation error mentioning "categories"

  @C15 @passing
  Scenario: the built-in stub needs no secret and is flagged built-in
    Given the indexer settings row:
      """
      { "kind": "stub", "name": "built-in" }
      """
    When the provider factory builds the indexer
    Then the indexer builds
    And the settings row is a built-in indexer

  @C15 @passing
  Scenario: a cardigann row is built from its catalog definition
    Given the indexer settings row:
      """
      { "kind": "cardigann", "name": "Synth", "definition_id": "synthjson", "settings": { "apiurl": "x" } }
      """
    When the provider factory builds the indexer
    Then the indexer builds
    And the settings row round-trips with name "Synth"

  @C15 @passing
  Scenario: an unknown protocol kind (newznab) is not a valid settings row yet
    Given the indexer settings row:
      """
      { "kind": "newznab", "name": "usenet", "base_url": "https://x", "categories": [2000] }
      """
    When the provider factory builds the indexer
    Then the settings row is rejected as an unknown kind

  @C15 @passing
  Scenario: the per-indexer rate cap is optional and round-trips
    Given the indexer settings row:
      """
      { "kind": "prowlarr", "name": "pw", "base_url": "http://gluetun:9696", "categories": [2000], "rate_per_minute": 30 }
      """
    When the provider factory builds the indexer
    Then the indexer builds
    And the stored settings row carries the per-indexer field "rate_per_minute"

  @C15 @passing @SKADI-T-0505
  Scenario: a settings row carries Sonarr's per-indexer flags (enable RSS / automatic search / interactive search)
    Given the indexer settings row:
      """
      { "kind": "torznab", "name": "geek", "base_url": "https://x", "categories": [2000] }
      """
    Then the stored settings row carries the per-indexer field "enable_rss"
    And the stored settings row carries the per-indexer field "enable_automatic_search"

  @C15 @passing @SKADI-T-0505
  Scenario: a settings row carries an indexer priority and minimum seeders
    Given the indexer settings row:
      """
      { "kind": "torznab", "name": "geek", "base_url": "https://x", "categories": [2000] }
      """
    Then the stored settings row carries the per-indexer field "priority"
    And the stored settings row carries the per-indexer field "minimum_seeders"

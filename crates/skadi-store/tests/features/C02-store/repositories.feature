Feature: Persistence — settings, domains, JSON columns and timestamps (C02)
  The generic (kind, id) -> JSON settings store and the per-domain enable
  state, written once against the dual-backend connection. Behaviour must be
  identical on SQLite (TEXT json / RFC3339 text) and Postgres (jsonb /
  timestamptz).

  @C02 @passing
  Scenario: a settings document is identified by kind and id; re-storing updates in place
    Given a migrated sqlite store
    When the indexers setting "i1" is stored with body:
      """
      { "name": "nzbgeek", "categories": [2000, 5000], "nested": { "unicode": "Skådi ☃", "big": 9007199254740993, "float": 1.5, "null": null } }
      """
    Then the indexers kind holds exactly 1 setting
    And the indexers setting "i1" has body:
      """
      { "name": "nzbgeek", "categories": [2000, 5000], "nested": { "unicode": "Skådi ☃", "big": 9007199254740993, "float": 1.5, "null": null } }
      """
    When the indexers setting "i1" is stored again with body:
      """
      { "name": "nzbgeek", "categories": [2000] }
      """
    Then the indexers kind holds exactly 1 setting
    And the indexers setting "i1" has body:
      """
      { "name": "nzbgeek", "categories": [2000] }
      """
    And the stored timestamps of indexers "i1" are UTC and at least microsecond-precise

  @C02 @passing
  Scenario: the same id under two kinds is two documents, listed per kind in id order
    Given a migrated sqlite store
    When the indexers setting "shared" is stored with body:
      """
      { "kind": "indexer" }
      """
    And the downloaders setting "shared" is stored with body:
      """
      { "kind": "downloader" }
      """
    And the indexers setting "aaa" is stored with body:
      """
      { "kind": "first" }
      """
    Then the indexers kind holds exactly 2 settings
    And the downloaders kind holds exactly 1 setting
    And the indexers settings list in id order "aaa, shared"
    When the indexers setting "shared" is deleted
    Then the delete reported true
    When the indexers setting "shared" is deleted
    Then the delete reported false
    And the downloaders setting "shared" has body:
      """
      { "kind": "downloader" }
      """

  @C02 @passing
  Scenario: a domain keeps its settings across enable and disable, and lists by name
    Given a migrated sqlite store
    When the domain "movies" is registered with settings:
      """
      { "root": "/movies" }
      """
    And the domain "movies" is enabled
    Then the domain "movies" is enabled with its settings intact
    When the domain "movies" is disabled
    Then the domain "movies" is disabled with its settings intact
    When the domain "audiobooks" is enabled
    Then the domains list in name order "audiobooks, movies"

  @C02 @bug @SKADI-T-0518
  Scenario: concurrent writers on a SQLite store all succeed (no busy timeout: 15 of 16 fail with "database is locked")
    Given a migrated sqlite store
    When 16 writers upsert distinct config keys at the same time
    Then every write succeeded and 16 keys are stored

  @C02 @passing @postgres
  Scenario: JSON documents and timestamps round-trip identically on Postgres
    Given a migrated postgres store
    When the indexers setting "i1" is stored with body:
      """
      { "name": "nzbgeek", "categories": [2000, 5000], "nested": { "unicode": "Skådi ☃", "big": 9007199254740993, "float": 1.5, "null": null } }
      """
    And the indexers setting "i1" is stored again with body:
      """
      { "name": "nzbgeek", "categories": [2000] }
      """
    Then the indexers setting "i1" has body:
      """
      { "name": "nzbgeek", "categories": [2000] }
      """
    And the stored timestamps of indexers "i1" are UTC and at least microsecond-precise
    When the domain "movies" is registered with settings:
      """
      { "root": "/movies" }
      """
    And the domain "movies" is enabled
    Then the domain "movies" is enabled with its settings intact

  @C02 @passing @postgres
  Scenario: concurrent writers on a Postgres store all succeed
    Given a migrated postgres store
    When 16 writers upsert distinct config keys at the same time
    Then every write succeeded and 16 keys are stored

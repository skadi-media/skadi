Feature: Credential vault — provider secrets through the daemon (C04)
  Settings create/update strip the secret field and seal it in the vault; the
  provider factory pulls it back when building live indexers/downloaders/
  notifiers. A row without a required secret is skipped, never fatal.

  Background:
    Given a daemon running in open mode

  @C04 @passing @serial
  Scenario: an API key saved through the settings endpoint is sealed at rest
    Given the process environment sets "SKADI_SECRET_KEY" to "correct horse battery staple"
    And the daemon's store is opened under that key
    And a stored indexers setting remembered as "ix" with body:
      """
      { "kind": "torznab", "name": "nzbgeek", "base_url": "http://127.0.0.1:1", "categories": [2000], "api_key": "abc" }
      """
    Then the credential for indexers "ix" is sealed at rest

  @C04 @passing
  Scenario: rows that lack a required secret are skipped while the rest of the set builds
    Given a stored indexers setting remembered as "keyless" with body:
      """
      { "kind": "torznab", "name": "keyless", "base_url": "http://127.0.0.1:1", "categories": [2000] }
      """
    And a stored indexers setting remembered as "keyed" with body:
      """
      { "kind": "torznab", "name": "keyed", "base_url": "http://127.0.0.1:1", "categories": [2000], "api_key": "abc" }
      """
    And a stored downloaders setting remembered as "builtin" with body:
      """
      { "kind": "skadi", "name": "built-in" }
      """
    When the provider set is built from the store
    Then the provider set holds 1 indexer, 1 downloader and 0 notifiers

  @C04 @passing @SKADI-T-0519 @serial
  Scenario: a credential sealed under a previous master key is skipped rather than aborting the whole provider build
    Given the process environment sets "SKADI_SECRET_KEY" to "key-a"
    And the daemon's store is opened under that key
    And a stored indexers setting remembered as "ix" with body:
      """
      { "kind": "torznab", "name": "nzbgeek", "base_url": "http://127.0.0.1:1", "categories": [2000], "api_key": "abc" }
      """
    When the daemon restarts with the master key "key-b"
    And the restarted daemon builds the provider set
    Then the provider set built with the unreadable credential skipped

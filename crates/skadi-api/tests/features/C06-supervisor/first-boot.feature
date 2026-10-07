Feature: First-boot provisioning — a new install is usable with no CLI steps (C06)
  SKADI-T-0703. Each boot fills in only what is absent: the built-in
  downloader when there is no download client, the default quality profiles
  when there is no profile. On a fresh database only, it also enables each
  domain whose folder under library.root exists or can be created. An existing
  install is never changed, and a second boot changes nothing.
  The curated public trackers keep the behaviour they had before: added when
  missing and pruned when out of scope, on boot and after each definition sync,
  unless SKADI_DEFAULT_INDEXERS is false.

  Background:
    Given a daemon running in open mode
    And the cardigann definitions live in a scratch folder

  @C06 @passing @serial
  Scenario: a fresh boot with a library root provisions a working install
    Given the library root is an empty folder
    When the daemon bootstraps with the "probe" domain compiled in
    Then exactly 1 built-in downloader is configured
    And the built-in downloader downloads under the library root
    And the profiles settings kind holds 6 documents
    And the domain "probe" is enabled
    And the folder "movie" exists under the library root

  @C06 @passing @serial
  Scenario: a second boot changes nothing
    Given the library root is an empty folder
    When the daemon bootstraps with the "probe" domain compiled in
    And the state of the install is recorded
    And the daemon bootstraps with the "probe" domain compiled in
    Then the install is unchanged since it was recorded

  @C06 @passing @serial
  Scenario: an existing install is not changed when a library root appears
    When the daemon bootstraps with the "probe" domain compiled in
    And the operator renames the built-in downloader to "mine"
    And the state of the install is recorded
    Given the library root is an empty folder
    When the daemon bootstraps with the "probe" domain compiled in
    Then the domain "probe" is registered but disabled
    And the install is unchanged since it was recorded

  @C06 @passing @serial
  Scenario: a library root that does not exist enables no domain
    Given the library root is a folder that does not exist
    When the daemon bootstraps with the "probe" domain compiled in
    Then the domain "probe" is registered but disabled
    And exactly 1 built-in downloader is configured

  @C06 @passing @serial
  Scenario: the operator's profiles and download client are kept
    Given the operator has a profile named "mine"
    And the operator has a download client named "theirs"
    When the daemon bootstraps with the "probe" domain compiled in
    Then the profiles settings kind holds 1 document
    And the downloaders settings kind holds 1 document

  @C06 @passing @serial
  Scenario: with the switch unset, boot and definition sync add the curated trackers and prune an out-of-scope one
    Given an auto-seeded indexer for "nyaasi"
    And an operator-added indexer for "torrentleech"
    When the daemon bootstraps with the "probe" domain compiled in
    Then the indexer "thepiratebay" is registered from the curated set
    And the indexer "audiobookbay" is registered from the curated set
    And no indexer for "nyaasi" is registered
    And the indexer for "torrentleech" is still registered
    When a definition sync brings the new public tracker "newtracker"
    Then the indexer "newtracker" is registered from the curated set
    When the operator removes the indexer "yts"
    And a definition sync brings the new public tracker "othertracker"
    Then the indexer "yts" is registered from the curated set

  @C06 @passing @serial
  Scenario: SKADI_DEFAULT_INDEXERS=false neither adds nor prunes trackers
    Given the curated trackers are switched off
    And an auto-seeded indexer for "nyaasi"
    When the daemon bootstraps with the "probe" domain compiled in
    And a definition sync brings the new public tracker "newtracker"
    Then the indexers settings kind holds 1 document
    And the indexer for "nyaasi" is still registered

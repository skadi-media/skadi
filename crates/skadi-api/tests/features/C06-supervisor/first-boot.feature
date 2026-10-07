Feature: First-boot provisioning — a new install is usable with no CLI steps (C06)
  SKADI-T-0703. Each boot fills in only what is absent: the built-in
  downloader when there is no download client, the default quality profiles
  when there is no profile. On a fresh database only, it also enables each
  domain whose folder under library.root exists or can be created. The default
  indexer set is opt-in (SKADI_DEFAULT_INDEXERS). An existing install is never
  changed, and a second boot changes nothing.

  Background:
    Given a daemon running in open mode

  @C06 @passing @serial
  Scenario: a fresh boot with a library root provisions a working install
    Given the library root is an empty folder
    When the daemon bootstraps with the "probe" domain compiled in
    Then exactly 1 built-in downloader is configured
    And the built-in downloader downloads under the library root
    And the profiles settings kind holds 6 documents
    And the domain "probe" is enabled
    And the folder "movie" exists under the library root
    And the indexers settings kind holds 0 documents

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
  Scenario: the default indexer set is registered only when it is switched on
    Given the default indexers are switched on
    When the daemon bootstraps with the "probe" domain compiled in
    Then the indexers settings kind holds 6 documents
    And the indexer "thepiratebay" is registered from the default set

  @C06 @passing @serial
  Scenario: a default indexer that the operator removed does not come back
    Given the default indexers are switched on
    When the daemon bootstraps with the "probe" domain compiled in
    And the operator removes the indexer "yts"
    And the state of the install is recorded
    And the daemon bootstraps with the "probe" domain compiled in
    Then the indexers settings kind holds 5 documents
    And the install is unchanged since it was recorded

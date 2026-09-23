Feature: Audiobook wanted, upgrade and reconcile sweeps (C24 catalog, AudiobookWantedQuery)
  Readarr's "Wanted: Missing" / "Cutoff Unmet" over Book -> BookFile.

  Background:
    Given an empty audiobook library
    And an audiobook profile allowing "MP3-64" and "M4B-256" with cutoff "M4B-256"

  @C24 @passing
  Scenario: a monitored book with a Missing file is wanted with an author-qualified, series-aware seed
    Given a monitored book "The Way of Kings" by "Brandon Sanderson" with ASIN B003 that is Missing
    And the book B003 belongs to series "Stormlight Archive" at position "1"
    When the wanted sweep runs
    Then 1 seed is emitted
    And the seed for B003 carries no current quality
    And the seed for B003 searches the audiobook category with year 2010
    And the seed for B003 lists titles "The Way of Kings, The Way of Kings Brandon Sanderson, Stormlight Archive"
    And the seed for B003 carries series "Stormlight Archive"

  @C24 @passing
  Scenario: an unmonitored book is never wanted or upgradable
    Given an unmonitored book "Quiet" by "Nobody" with ASIN B0Q1 that is Missing
    And an unmonitored book "Held" by "Nobody" with ASIN B0Q2 imported at "MP3-64"
    When the wanted sweep runs
    Then no seed is emitted
    When the upgrade sweep runs
    Then no seed is emitted

  @C24 @passing
  Scenario: a download failure is retried after its backoff, a no-release failure is not
    Given a monitored book "Flaky" by "A" with ASIN B0F1 that failed a download with retry due 1 minutes ago
    And a monitored book "Waiting" by "A" with ASIN B0F2 that failed a download with retry due in 60 minutes
    And a monitored book "Obscure" by "A" with ASIN B0F3 that failed with no suitable release
    When the wanted sweep runs
    Then 1 seed is emitted
    And the seed for B0F1 carries no current quality

  @C24 @passing
  Scenario: a file imported below the cutoff is upgradable with its held quality
    Given a monitored book "Low" by "A" with ASIN B0L1 imported at "MP3-64"
    And a monitored book "High" by "A" with ASIN B0H1 imported at "M4B-256"
    When the wanted sweep runs
    Then no seed is emitted
    When the upgrade sweep runs
    Then 1 seed is emitted
    And the seed for B0L1 carries current quality "MP3-64" and format score 0

  @C24 @passing
  Scenario: the profile can switch upgrades off
    Given the profile disallows upgrades
    And a monitored book "Low" by "A" with ASIN B0L1 imported at "MP3-64"
    When the upgrade sweep runs
    Then no seed is emitted

  @C24 @passing @SKADI-T-0399
  Scenario: a file held at the Unknown tier must not seed an upgrade under the built-in profile (SKADI-T-0399 family)
    Given the built-in audiobook profile with upgrades allowed
    And a monitored book "Untagged" by "A" with ASIN B0U1 imported at "Unknown"
    When the upgrade sweep runs
    Then no seed is emitted

  @C24 @passing @SKADI-T-0400
  Scenario: a second monitored edition of a title the library already holds is not wanted (SKADI-T-0400)
    Given a monitored book "Blood of Elves" by "Andrzej Sapkowski" with ASIN B0HGMP2KGZ imported at "M4B-256"
    And a monitored book "Blood of Elves" by "Andrzej Sapkowski" with ASIN B0HHD9X8NS that is Missing
    When the wanted sweep runs
    Then no seed is emitted

  @C24 @passing
  Scenario: a demoted import whose file is on disk is healed and not re-grabbed (SKADI-T-0385)
    Given a monitored book "Saturation Point" by "A" with ASIN B0S1 that is Missing
    And the file of B0S1 was imported at "MP3-64" to a library file on disk
    And the file of B0S1 was then overwritten to an import failure due for retry
    When the stale reconcile runs
    Then 1 file is recovered
    And the file of B0S1 is Imported
    And the file of B0S1 holds quality "MP3-64" and format score 7
    When the wanted sweep runs
    Then no seed is emitted

  @C24 @passing
  Scenario: a demoted import with no recorded quality is restored at the Unknown tier
    Given a monitored book "NoQual" by "A" with ASIN B0N1 whose Missing file still points at a library file with no recorded quality
    When the stale reconcile runs
    Then 1 file is recovered
    And the file of B0N1 holds quality "Unknown" and format score 7

  @C24 @passing
  Scenario: a demoted import whose file is gone stays wanted
    Given a monitored book "Gone" by "A" with ASIN B0G1 that is Missing
    And the file of B0G1 was imported at "MP3-64" to a library file on disk
    And the file of B0G1 was then overwritten to Missing
    And the library file of B0G1 was deleted
    When the stale reconcile runs
    Then 0 files are recovered
    When the wanted sweep runs
    Then 1 seed is emitted

  @C24 @passing
  Scenario: a wedged in-flight file with its file on disk goes straight back to Imported
    Given a monitored book "Wedged" by "A" with ASIN B0W1 that is Missing
    And the file of B0W1 was imported at "MP3-64" to a library file on disk
    And the file of B0W1 was then overwritten to Downloading 20 minutes ago
    When the stale reconcile runs
    Then 1 file is recovered
    And the file of B0W1 is Imported

  @C24 @passing
  Scenario: a wedged download past the grace with no file is reset to Missing, a fresh one is left alone
    Given a monitored book "Stale" by "A" with ASIN B0ST that has been Downloading for 20 minutes
    And a monitored book "Fresh" by "A" with ASIN B0FR that has been Downloading for 1 minutes
    When the stale reconcile runs
    Then 1 file is recovered
    And the file of B0ST is Missing
    And the file of B0FR is Downloading

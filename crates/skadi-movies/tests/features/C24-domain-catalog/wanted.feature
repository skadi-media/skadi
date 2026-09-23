Feature: Movies wanted, upgrade and reconcile sweeps (C24 catalog, MovieWantedQuery)
  Radarr's "Wanted: Missing" and "Wanted: Cutoff Unmet" over the Movie -> Edition model.

  Background:
    Given an empty movies library
    And a quality profile allowing "Bluray-720p" and "Bluray-1080p" with cutoff "Bluray-1080p"

  @C24 @passing
  Scenario: a monitored movie with a Missing edition is wanted with a first-acquisition seed
    Given a monitored movie "The Matrix" from 1999 with a Missing Theatrical edition
    When the wanted sweep runs
    Then 1 seed is emitted
    And the seed for "The Matrix" carries no current quality
    And the seed for "The Matrix" searches category 2000 with year 1999
    And the seed for "The Matrix" lists titles "The Matrix, The.Matrix"
    And the seed for "The Matrix" carries the movie's TMDB id

  @C24 @passing
  Scenario: an unmonitored movie is never wanted or upgradable
    Given an unmonitored movie "Off the Books" from 2010 with a Missing Theatrical edition
    And an unmonitored movie "Quiet" from 2001 with a Theatrical edition imported at "Bluray-720p"
    When the wanted sweep runs
    Then no seed is emitted
    When the upgrade sweep runs
    Then no seed is emitted

  @C24 @passing
  Scenario: a download failure is retried once its backoff has elapsed
    Given a monitored movie "Flaky" from 2005 whose Theatrical edition failed a download with retry due 1 minutes ago
    And a monitored movie "Waiting" from 2006 whose Theatrical edition failed a download with retry due in 60 minutes
    When the wanted sweep runs
    Then 1 seed is emitted
    And the seed for "Flaky" carries no current quality

  @C24 @passing
  Scenario: a no-suitable-release failure is not auto-retried by the sweep
    Given a monitored movie "Obscure" from 1971 whose Theatrical edition failed with no suitable release
    When the wanted sweep runs
    Then no seed is emitted
    And the Theatrical edition of "Obscure" is wanted by the acquirable contract

  @C24 @passing
  Scenario: an edition imported below the cutoff is upgradable and carries its held quality
    Given a monitored movie "The Matrix" from 1999 with a Theatrical edition imported at "Bluray-720p" with format score 3
    When the wanted sweep runs
    Then no seed is emitted
    When the upgrade sweep runs
    Then 1 seed is emitted
    And the seed for "The Matrix" carries current quality "Bluray-720p" and format score 3

  @C24 @passing
  Scenario: an edition imported at the cutoff is not upgradable
    Given a monitored movie "The Matrix" from 1999 with a Theatrical edition imported at "Bluray-1080p"
    When the upgrade sweep runs
    Then no seed is emitted

  @C24 @passing
  Scenario: the profile can switch upgrades off entirely
    Given the profile disallows upgrades
    And a monitored movie "The Matrix" from 1999 with a Theatrical edition imported at "Bluray-720p"
    When the upgrade sweep runs
    Then no seed is emitted

  @C24 @passing
  Scenario: opting into a format-score cutoff re-checks an at-cutoff edition for a proper
    Given the profile re-checks imports until format score 100
    And a monitored movie "The Matrix" from 1999 with a Theatrical edition imported at "Bluray-1080p" with format score 0
    When the upgrade sweep runs
    Then 1 seed is emitted
    And the seed for "The Matrix" carries current quality "Bluray-1080p" and format score 0

  @C24 @passing
  Scenario: a quality outside the profile's allowed list ranks as below cutoff, like Radarr
    Given a monitored movie "Old DVD" from 1990 with a Theatrical edition imported at "DVD"
    When the upgrade sweep runs
    Then 1 seed is emitted

  @C24 @passing @SKADI-T-0399
  Scenario: an adopted file whose quality was never assessed must not seed an upgrade (SKADI-T-0399)
    Given a monitored movie "101 Dalmatians" from 1996 with a Theatrical edition adopted at the unassessed default quality
    When the upgrade sweep runs
    Then no seed is emitted

  @C24 @passing @SKADI-T-0399
  Scenario: restoring a demoted import with no recorded quality must not fabricate the lowest tier (SKADI-T-0399)
    Given a monitored movie "Adopted" from 1996 whose Missing Theatrical edition still points at a library file with no recorded quality
    When the stale reconcile runs
    Then 1 edition is recovered
    And the Theatrical edition of "Adopted" is Imported
    And the Theatrical edition of "Adopted" holds a quality other than "SDTV"

  @C24 @passing
  Scenario: a demoted import whose file is still on disk is healed instead of re-grabbed (SKADI-T-0385/0388)
    Given a monitored movie "101 Dalmatians" from 1996 with a Missing Theatrical edition
    And the Theatrical edition of "101 Dalmatians" was imported at "Bluray-720p" to a library file on disk
    And the edition of "101 Dalmatians" was then overwritten to Missing
    When the stale reconcile runs
    Then 1 edition is recovered
    And the Theatrical edition of "101 Dalmatians" is Imported
    And the Theatrical edition of "101 Dalmatians" holds quality "Bluray-720p" and format score 7
    When the wanted sweep runs
    Then no seed is emitted

  @C24 @passing
  Scenario: a demoted import whose file is gone stays wanted
    Given a monitored movie "Lost" from 2004 with a Missing Theatrical edition
    And the Theatrical edition of "Lost" was imported at "Bluray-720p" to a library file on disk
    And the edition of "Lost" was then overwritten to Missing
    And the library file of "Lost" was deleted
    When the stale reconcile runs
    Then 0 editions are recovered
    And the Theatrical edition of "Lost" is Missing
    When the wanted sweep runs
    Then 1 seed is emitted

  @C24 @passing
  Scenario: a wedged in-flight edition with its file on disk goes straight back to Imported
    Given a monitored movie "Wedged" from 2015 with a Missing Theatrical edition
    And the Theatrical edition of "Wedged" was imported at "Bluray-720p" to a library file on disk
    And the edition of "Wedged" was then overwritten to Searching 20 minutes ago
    When the stale reconcile runs
    Then 1 edition is recovered
    And the Theatrical edition of "Wedged" is Imported
    When the wanted sweep runs
    Then no seed is emitted

  @C24 @passing
  Scenario: a wedged download past the grace with no file is reset to Missing, a fresh one is left alone
    Given a monitored movie "Stale" from 2020 whose Theatrical edition has been Downloading for 20 minutes
    And a monitored movie "Fresh" from 2021 whose Theatrical edition has been Downloading for 1 minutes
    When the stale reconcile runs
    Then 1 edition is recovered
    And the Theatrical edition of "Stale" is Missing
    And the Theatrical edition of "Fresh" is Downloading
    When the wanted sweep runs
    Then 1 seed is emitted

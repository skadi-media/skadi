Feature: Television wanted, upgrade and reconcile sweeps (C24 catalog, SeriesWantedQuery)
  Sonarr's "Wanted: Missing" / "Cutoff Unmet" at episode grain, plus season-pack seeds.

  Background:
    Given an empty television library
    And a quality profile allowing "Bluray-720p" and "Bluray-1080p" with cutoff "Bluray-1080p"
    And a monitored series "Game of Thrones" from 2011

  @C24 @passing
  Scenario: an aired, monitored, Missing episode is wanted with a tv-search scoped seed
    Given "Game of Thrones" has episode S1E1 aired on 2011-04-17 that is Missing
    When the wanted sweep runs
    Then 1 seed is emitted
    And the seed for S1E1 of "Game of Thrones" carries no current quality
    And the seed for S1E1 of "Game of Thrones" scopes season 1 episode 1 in TV category 5000
    And the seed for S1E1 of "Game of Thrones" carries air date 2011-04-17
    And the seed for S1E1 of "Game of Thrones" lists titles "Game of Thrones, Game of Thrones (2011), Game of Thrones 2011, Game.of.Thrones"

  @C24 @passing
  Scenario: un-aired and unmonitored episodes are skipped
    Given "Game of Thrones" has episode S1E1 aired on 2011-04-17 that is Missing
    And "Game of Thrones" has episode S9E1 aired on 2999-01-01 that is Missing
    And "Game of Thrones" has an unmonitored episode S1E3 aired on 2011-05-01
    When the wanted sweep runs
    Then 1 seed is emitted

  @C24 @passing
  Scenario: an unmonitored series yields nothing even with Missing episodes
    Given an unmonitored series "Quiet" from 2001
    And "Quiet" has episode S1E1 aired on 2001-01-01 that is Missing
    And "Quiet" has episode S1E2 aired on 2001-01-08 imported at "Bluray-720p"
    When the wanted sweep runs
    Then no seed is emitted
    When the upgrade sweep runs
    Then no seed is emitted

  @C24 @passing
  Scenario: a download failure is retried after its backoff, a no-release failure is not
    Given "Game of Thrones" has episode S1E1 aired on 2011-04-17 that failed a download with retry due 1 minutes ago
    And "Game of Thrones" has episode S1E2 aired on 2011-04-24 that failed a download with retry due in 60 minutes
    And "Game of Thrones" has episode S1E3 aired on 2011-05-01 that failed with no suitable release
    When the wanted sweep runs
    # S1E1 is the only episode due — but three S1 episodes are unsatisfied, so
    # the pack search rides along with it (SKADI-T-0607).
    Then 2 seeds are emitted
    And the first seed is a season pack for season 1 of "Game of Thrones"
    And the seed for S1E1 of "Game of Thrones" carries no current quality

  @C24 @passing
  Scenario: a season pack seed counts backed-off episodes, but needs one that is due (SKADI-T-0607)
    Given "Game of Thrones" has episode S1E1 aired on 2011-04-17 that is Missing
    And "Game of Thrones" has episode S1E2 aired on 2011-04-24 that failed a download with retry due in 60 minutes
    And "Game of Thrones" has episode S1E3 aired on 2011-05-01 that failed a download with retry due in 60 minutes
    And "Game of Thrones" has episode S2E1 aired on 2012-04-01 that failed a download with retry due in 60 minutes
    And "Game of Thrones" has episode S2E2 aired on 2012-04-08 that failed a download with retry due in 60 minutes
    And "Game of Thrones" has episode S2E3 aired on 2012-04-15 that failed a download with retry due in 60 minutes
    When the wanted sweep runs
    Then 2 seeds are emitted
    And the first seed is a season pack for season 1 of "Game of Thrones"
    And no season pack seed is emitted for season 2

  @C24 @passing
  Scenario: three or more missing episodes in a season also emit a season-pack seed first
    Given "Game of Thrones" has episodes S1E1 through E4 aired on 2011-04-17 that are Missing
    And "Game of Thrones" has episodes S2E1 through E2 aired on 2012-04-01 that are Missing
    When the wanted sweep runs
    Then 7 seeds are emitted
    And the first seed is a season pack for season 1 of "Game of Thrones"
    And no season pack seed is emitted for season 2

  @C24 @passing
  Scenario: specials never get a season-pack seed (SKADI-T-0386)
    Given "Game of Thrones" has episodes S0E1 through E3 aired on 2011-04-17 that are Missing
    When the wanted sweep runs
    Then 3 seeds are emitted
    And no season pack seed is emitted for season 0

  @C24 @passing
  Scenario: an anime episode seed carries the absolute number for the identity gate
    Given "Game of Thrones" is an anime series
    And "Game of Thrones" has episode S1E28 aired on 2011-04-17 with absolute number 28
    When the wanted sweep runs
    Then the seed for S1E28 of "Game of Thrones" carries absolute number 28

  @C24 @passing
  Scenario: an episode imported below the cutoff is upgradable with its held quality and score
    Given "Game of Thrones" has episode S1E1 aired on 2011-04-17 imported at "Bluray-720p" with format score 3
    And "Game of Thrones" has episode S1E2 aired on 2011-04-24 imported at "Bluray-1080p"
    When the wanted sweep runs
    Then no seed is emitted
    When the upgrade sweep runs
    Then 1 seed is emitted
    And the seed for S1E1 of "Game of Thrones" carries current quality "Bluray-720p" and format score 3

  @C24 @passing
  Scenario: the profile can switch upgrades off
    Given the profile disallows upgrades
    And "Game of Thrones" has episode S1E1 aired on 2011-04-17 imported at "Bluray-720p"
    When the upgrade sweep runs
    Then no seed is emitted

  @C24 @passing @SKADI-T-0399
  Scenario: an adopted episode whose quality was never assessed must not seed an upgrade (SKADI-T-0399)
    Given "Game of Thrones" has episode S1E1 aired on 2011-04-17 adopted at the unassessed default quality
    When the upgrade sweep runs
    Then no seed is emitted

  @C24 @passing @SKADI-T-0399
  Scenario: restoring a demoted episode with no recorded quality must not fabricate the lowest tier (SKADI-T-0399)
    Given "Game of Thrones" has a Missing episode S1E1 aired on 2011-04-17 that still points at a library file with no recorded quality
    When the stale reconcile runs
    Then 1 episode is recovered
    And episode S1E1 of "Game of Thrones" is Imported
    And episode S1E1 of "Game of Thrones" holds a quality other than "SDTV"

  @C24 @passing
  Scenario: a demoted import whose file is on disk is healed and not re-grabbed (12 Monkeys S01E01–04)
    Given "Game of Thrones" has episode S1E1 aired on 2011-04-17 that is Missing
    And episode S1E1 of "Game of Thrones" was imported at "Bluray-720p" to a library file on disk
    And episode S1E1 of "Game of Thrones" was then overwritten to Missing
    When the stale reconcile runs
    Then 1 episode is recovered
    And episode S1E1 of "Game of Thrones" is Imported
    And episode S1E1 of "Game of Thrones" holds quality "Bluray-720p" and format score 7
    When the wanted sweep runs
    Then no seed is emitted

  @C24 @passing
  Scenario: a demoted import whose file is gone stays wanted
    Given "Game of Thrones" has episode S1E2 aired on 2011-04-24 that is Missing
    And episode S1E2 of "Game of Thrones" was imported at "Bluray-720p" to a library file on disk
    And episode S1E2 of "Game of Thrones" was then overwritten to Missing
    And the library file of episode S1E2 of "Game of Thrones" was deleted
    When the stale reconcile runs
    Then 0 episodes are recovered
    When the wanted sweep runs
    Then 1 seed is emitted

  @C24 @passing
  Scenario: a wedged in-flight episode with its file on disk goes straight back to Imported
    Given "Game of Thrones" has episode S1E3 aired on 2011-05-01 that is Missing
    And episode S1E3 of "Game of Thrones" was imported at "Bluray-720p" to a library file on disk
    And episode S1E3 of "Game of Thrones" was then overwritten to Searching 20 minutes ago
    When the stale reconcile runs
    Then 1 episode is recovered
    And episode S1E3 of "Game of Thrones" is Imported
    When the wanted sweep runs
    Then no seed is emitted

  @C24 @passing
  Scenario: a wedged download past the grace with no file is reset to Missing, a fresh one is left alone
    Given "Game of Thrones" has episode S1E1 aired on 2011-04-17 that has been Downloading for 20 minutes
    And "Game of Thrones" has episode S1E2 aired on 2011-04-24 that has been Downloading for 1 minutes
    When the stale reconcile runs
    Then 1 episode is recovered
    And episode S1E1 of "Game of Thrones" is Missing
    And episode S1E2 of "Game of Thrones" is Downloading

  @C24 @passing @SKADI-T-0446
  Scenario: an episode with no known air date is not searched (Sonarr skips episodes without an air date)
    Given "Game of Thrones" has episode S1E9 with no air date that is Missing
    When the wanted sweep runs
    Then no seed is emitted

  @C24 @passing @SKADI-T-0446
  Scenario: the operator can opt in to searching undated episodes (Sonarr "search for undated")
    Given the operator opts in to searching undated episodes
    And "Game of Thrones" has episode S1E9 with no air date that is Missing
    When the wanted sweep runs
    Then 1 seed is emitted

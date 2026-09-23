Feature: Television catalog CRUD and the LibraryItem / Acquirable contract (C24)
  Sonarr's Series -> Season -> Episode library as Skadi's Series -> Episode acquirables.

  Background:
    Given an empty television library

  @C24 @passing
  Scenario: listing filters on the monitored flag and hydrates episodes
    Given a monitored series "Game of Thrones" from 2011
    And "Game of Thrones" has episodes S1E1 through E2 aired on 2011-04-17 that are Missing
    And an unmonitored series "Quiet" from 2001
    When the library is listed with monitored filter true
    Then the listing contains "Game of Thrones" with 2 episodes
    And the listing does not contain "Quiet"
    When the library is listed with monitored filter any
    Then the listing contains "Quiet" with 0 episodes

  @C24 @passing
  Scenario: a series is found by its TVDB id with seasons and episodes attached
    Given a monitored series "Game of Thrones" from 2011
    And "Game of Thrones" has episode S1E1 aired on 2011-04-17 that is Missing
    When season 1 of "Game of Thrones" is recorded
    And the series "Game of Thrones" is looked up by its TVDB id
    Then the lookup returns "Game of Thrones" with 1 episode and 1 season

  @C24 @passing
  Scenario: a series without a TVDB id cannot be saved
    When a series without a TVDB id is saved
    Then the save is rejected as a validation error

  @C24 @passing
  Scenario: deleting a series removes its seasons and episodes on both backends
    Given a monitored series "Gone" from 2000
    And "Gone" has episodes S1E1 through E3 aired on 2000-01-01 that are Missing
    When season 1 of "Gone" is recorded
    And the series "Gone" is deleted
    And the series "Gone" is loaded by id
    Then the lookup returns nothing
    And no episode or season rows remain for "Gone"

  @C24 @passing
  Scenario: the Acquirable contract requires the episode itself to be monitored
    Given a monitored series "Game of Thrones" from 2011
    And "Game of Thrones" has episode S1E1 aired on 2011-04-17 that is Missing
    And "Game of Thrones" has episode S1E2 aired on 2011-04-24 imported at "Bluray-1080p"
    Then episode S1E1 of "Game of Thrones" is wanted by the acquirable contract
    And episode S1E2 of "Game of Thrones" is unwanted by the acquirable contract
    When episode S1E1 of "Game of Thrones" is unmonitored
    Then episode S1E1 of "Game of Thrones" is unwanted by the acquirable contract

  @C24 @passing
  Scenario: the repo-level season toggle does not cascade to episodes (the HTTP route does)
    Given a monitored series "Game of Thrones" from 2011
    And "Game of Thrones" has episodes S1E1 through E2 aired on 2011-04-17 that are Missing
    When season 1 of "Game of Thrones" is recorded
    And season 1 of "Game of Thrones" is unmonitored at the repo
    Then season 1 of "Game of Thrones" is unmonitored but its episodes still are monitored

  @C24 @passing
  Scenario: the television domain contributes items to the unified library
    Given a monitored series "Game of Thrones" from 2011
    And "Game of Thrones" has episodes S1E1 through E2 aired on 2011-04-17 that are Missing
    And an unmonitored series "Quiet" from 2001
    When the unified library lists series with monitored filter true
    Then the library items include "Game of Thrones" with 2 edition rows
    And the library items do not include "Quiet"

Feature: Television metadata sync — add and refresh a series from the provider (C26)
  Sonarr's AddSeries with monitor options and RefreshSeries with new-episode discovery.

  Background:
    Given an empty television library
    And the series provider returns "Game of Thrones" first aired 2011-04-17 on "HBO"
    And the provider lists episode S0E1 "Inside GoT" airing 2011-04-10
    And the provider lists episode S1E1 "Winter Is Coming" airing 2011-04-17
    And the provider lists episode S1E2 "The Kingsroad" airing 2011-04-24
    And the provider lists episode S2E1 "The North Remembers" airing 2999-04-01

  @C26 @passing
  Scenario: adding with monitor mode all monitors every regular episode but not specials
    When "Game of Thrones" is added by TVDB id 121361 with monitor mode all
    Then the series "Game of Thrones" is monitored and typed standard
    And the series "Game of Thrones" carries network "HBO" and a poster
    And the series "Game of Thrones" has 4 episodes and 3 seasons
    And episode S1E1 of "Game of Thrones" is monitored
    And episode S2E1 of "Game of Thrones" is monitored
    And episode S0E1 of "Game of Thrones" is unmonitored
    And season 0 of "Game of Thrones" is unmonitored
    And season 1 of "Game of Thrones" is monitored

  @C26 @passing
  Scenario Outline: Sonarr's monitor options select which episodes start monitored
    When "Game of Thrones" is added by TVDB id 121361 with monitor mode <mode>
    Then episode S1E1 of "Game of Thrones" is <s1e1>
    And episode S1E2 of "Game of Thrones" is <s1e2>
    And episode S2E1 of "Game of Thrones" is <s2e1>

    Examples:
      | mode        | s1e1        | s1e2        | s2e1        |
      | future      | unmonitored | unmonitored | monitored   |
      | missing     | monitored   | monitored   | unmonitored |
      | existing    | unmonitored | unmonitored | unmonitored |
      | firstSeason | monitored   | monitored   | unmonitored |
      | lastSeason  | unmonitored | unmonitored | monitored   |
      | pilot       | monitored   | unmonitored | unmonitored |
      | none        | unmonitored | unmonitored | unmonitored |

  @C26 @passing
  Scenario: monitor mode none adds the series unmonitored
    When "Game of Thrones" is added by TVDB id 121361 with monitor mode none
    Then the series "Game of Thrones" is unmonitored and typed standard

  @C26 @passing
  Scenario: an anime flag from the provider types the series as anime
    Given the provider flags the series as anime
    And the provider lists episode S1E3 "Lord Snow" with absolute number 3
    When "Game of Thrones" is added by TVDB id 121361 with monitor mode all
    Then the series "Game of Thrones" is monitored and typed anime

  @C26 @passing
  Scenario: adding an already-known TVDB id returns the existing series instead of failing
    When "Game of Thrones" is added by TVDB id 121361 with monitor mode all
    And "Game of Thrones" is added again by TVDB id 121361 with monitor mode none
    Then only one series row exists for "Game of Thrones"
    And the series "Game of Thrones" is monitored and typed standard

  @C26 @passing
  Scenario: a refresh keeps user state and statuses, and adds newly listed episodes under the season's monitor flag
    When "Game of Thrones" is added by TVDB id 121361 with monitor mode all
    Given episode S1E2 of "Game of Thrones" was unmonitored by the user
    And episode S1E1 of "Game of Thrones" was imported at "Bluray-1080p"
    And the provider lists episode S1E3 "Lord Snow" airing 2011-05-01
    And the provider lists episode S0E2 "Making Of" airing 2011-05-02
    When the series "Game of Thrones" is refreshed from the provider
    Then the series "Game of Thrones" has 6 episodes and 3 seasons
    And episode S1E2 of "Game of Thrones" is unmonitored
    And episode S1E1 of "Game of Thrones" is still Imported at "Bluray-1080p"
    And episode S1E3 of "Game of Thrones" is monitored
    And episode S0E2 of "Game of Thrones" is unmonitored
    And episode S1E3 of "Game of Thrones" is titled "Lord Snow"

  @C26 @passing @SKADI-T-0447
  Scenario: episodes the provider no longer lists are pruned on refresh (Sonarr removes them)
    When "Game of Thrones" is added by TVDB id 121361 with monitor mode all
    Given the provider no longer lists episode S1E2
    When the series "Game of Thrones" is refreshed from the provider
    Then the series "Game of Thrones" has 3 episodes and 3 seasons

  @C26 @passing @SKADI-T-0447
  Scenario: a dropped episode that holds a file is kept and unmonitored, never deleted
    When "Game of Thrones" is added by TVDB id 121361 with monitor mode all
    Given episode S1E2 of "Game of Thrones" was imported at "Bluray-720p" to a library file on disk
    And the provider no longer lists episode S1E2
    When the series "Game of Thrones" is refreshed from the provider
    Then episode S1E2 of "Game of Thrones" still exists but is unmonitored

  @C26 @passing @SKADI-T-0453
  Scenario: a daily show is typed daily so air-date naming and matching apply (Sonarr series type)
    Given the provider describes the series as a daily show on "Comedy Central"
    When "Game of Thrones" is added by TVDB id 121361 with monitor mode all
    Then the series "Game of Thrones" is monitored and typed daily

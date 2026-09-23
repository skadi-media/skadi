Feature: Episode matcher — which episode does a downloaded file satisfy (C25)
  Sonarr's import decision "release -> episode file", including multi-episode, anime and daily shapes.

  @C25 @passing
  Scenario: a SxxEyy file routes to its episode under the season folder
    Given a matcher for "Game of Thrones" from 2011 as a standard series with episodes "S1E5"
    When the downloaded file "Game.of.Thrones.S01E05.1080p.WEB-DL.x265-GRP.mkv" is matched
    Then the match routes to S1E5 of "Game of Thrones"
    And the destination contains "Season_01"
    And the destination contains "S01E05"

  @C25 @passing
  Scenario: a multi-episode file satisfies both episodes with one overwriting destination
    Given a matcher for "Game of Thrones" from 2011 as a standard series with episodes "S1E5, S1E6"
    When the downloaded file "Game.of.Thrones.S01E05E06.1080p.WEB.mkv" is matched
    Then 2 matches are emitted covering S1E5 and S1E6 of "Game of Thrones"
    And all matches overwrite on collision and share one destination
    And the destination contains "S01E05-E06"

  @C25 @passing
  Scenario: an anime file matches on its absolute number
    Given a matcher for "Frieren" from 2023 as an anime series with episodes "S1E28"
    And matcher episode S1E28 of "Frieren" has absolute number 28
    When the downloaded file "[SubsPlease] Frieren - 28 (1080p) [ABCD].mkv" is matched
    Then the match routes to S1E28 of "Frieren"

  @C25 @passing
  Scenario: a daily show file matches on its air date
    Given a matcher for "The Daily Show" from 1996 as a daily series with episodes "S2024E42"
    And matcher episode S2024E42 of "The Daily Show" aired on 2024-03-01
    When the downloaded file "The.Daily.Show.2024.03.01.1080p.WEB.h264-GRP.mkv" is matched
    Then the match routes to S2024E42 of "The Daily Show"

  @C25 @passing
  Scenario: the wrong season, samples and unparseable names yield no match
    Given a matcher for "Game of Thrones" from 2011 as a standard series with episodes "S1E5"
    When the downloaded file "Show.S02E05.1080p.mkv" is matched
    Then no match is emitted
    When the downloaded file "Show.S01E05.sample.mkv" is matched
    Then no match is emitted
    When the downloaded file "Some.Movie.2019.1080p.BluRay.mkv" is matched
    Then no match is emitted

  @C25 @passing
  Scenario: an upgrade supersedes the episode's existing file
    Given a matcher for "Game of Thrones" from 2011 as a standard series with episodes "S1E5"
    And matcher episode S1E5 of "Game of Thrones" already holds "/tv/Game_of_Thrones/Season_01/old.mkv"
    When the downloaded file "Game.of.Thrones.S01E05.1080p.WEB-DL.mkv" is matched
    Then the match supersedes "/tv/Game_of_Thrones/Season_01/old.mkv"

  @C25 @passing @SKADI-T-0445
  Scenario: a file from a different show is not routed to this series (Sonarr checks the series title)
    Given a matcher for "Game of Thrones" from 2011 as a standard series with episodes "S1E1"
    When the downloaded file "The.Wire.S01E01.1080p.BluRay.x264-GRP.mkv" is matched
    Then no match is emitted

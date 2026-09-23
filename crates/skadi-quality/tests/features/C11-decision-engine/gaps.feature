Feature: Quality engine — *arr parity gaps
  Parser and taxonomy behaviours Sonarr/Radarr have that the clean-room
  engine lacks. Each step asserts the expected behaviour and fails today.

  @C11 @passing @SKADI-T-0432
  Scenario: a bare "WEB" tag is a WEB-DL source
    Given the release title "Lioness.S03E02.1080p.WEB.h264-GRP"
    When it is parsed as a TV episode
    Then the source is "WEB"
    Given the release title "Lioness.S03E02.1080p.WEB.h264-GRP"
    When the title is classified
    Then it classifies as "WEBDL-1080p"

  @C11 @passing @SKADI-T-0432
  Scenario: a resolution-only release classifies to the HDTV tier instead of nothing
    Given the release title "[SubsPlease] Frieren - 38 (1080p) [ABCD1234]"
    When the title is classified
    Then it classifies as "HDTV-1080p"

  @C11 @gap
  Scenario: streaming-service and rip tags are recognised as sources
    Then a corpus of titles parses as expected:
      | title                                          | year | resolution | source |
      | Show.S01E01.1080p.AMZN.WEBRip.DDP5.1.x264-GRP  |      | 1080p      | WEBRip |
      | Movie.2019.1080p.HDRip.x264-GRP                | 2019 | 1080p      | HDRip  |
      | Movie.2019.HDTV.x264-GRP                       | 2019 |            | HDTV   |
      | Movie.2019.720p.PDTV.x264-GRP                  | 2019 | 720p       | PDTV   |

  @C11 @gap
  Scenario: a multi-season pack exposes every season it carries
    Given the release title "Show.S01-S04.1080p.BluRay.x264-GRP"
    When it is parsed as a TV episode
    Then it covers seasons 1 to 4

  @C11 @gap
  Scenario: the regression corpus has grown past a seed set (REQ-DECIDE.13)
    When the movie corpus is replayed
    Then the corpus holds at least 500 entries

  @C11 @passing @SKADI-T-0435
  Scenario: bracketed quality tags (YTS-style) are not stripped as repost tags
    Given the release title "Movie (2020) [1080p] [BluRay] [5.1] [YTS.MX]"
    When it is parsed as a movie
    Then the title is "Movie"
    And the year is 2020
    And the resolution is "1080p"
    And the source is "BluRay"

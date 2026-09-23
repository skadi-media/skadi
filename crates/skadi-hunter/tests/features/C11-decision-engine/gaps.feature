Feature: Decision engine — *arr parity gaps
  Behaviours Sonarr/Radarr have that `decide` lacks. Each step asserts the
  expected behaviour and fails honestly today.

  @C11 @gap
  Scenario: a release whose parsed title is a different film is rejected (SKADI-T-0401)
    Given a wanted movie "Insurgent" (2015) with the standard profile
    And a candidate "Insurgent.Diaries.2015.1080p.BluRay.x264-GRP" with 50 seeders
    When the hunter decides
    Then no release is chosen

  @C11 @passing @SKADI-T-0438
  Scenario: a PROPER at the same quality is preferred without a custom format (Sonarr revision upgrade)
    Given a wanted movie "Heat" (1995) with the standard profile
    And the candidate releases:
      | title                                   | seeders |
      | Heat.1995.1080p.BluRay.x264-GRP         | 900     |
      | Heat.1995.PROPER.1080p.BluRay.x264-GRP  | 5       |
    When the hunter decides
    Then it chooses "Heat.1995.PROPER.1080p.BluRay.x264-GRP"

  @C11 @gap
  Scenario: a multi-season pack covering the wanted season is accepted (SKADI-T-0386 follow-up)
    Given a wanted episode "Show" S03E02 with the standard profile
    And a candidate "Show.S01-S04.1080p.BluRay.x264-GRP" with 50 seeders
    When the hunter decides
    Then it chooses "Show.S01-S04.1080p.BluRay.x264-GRP"

  @C11 @gap
  Scenario: a same-named reboot is rejected by the show's year (SKADI-T-0386 residual)
    Given a wanted episode "Battlestar Galactica" S01E01 with the standard profile
    And the show premiered in 1978
    And the candidate releases:
      | title                                                    | seeders |
      | Battlestar.Galactica.2004.S01E01.1080p.BluRay.x264-GRP   | 900     |
      | Battlestar.Galactica.1978.S01E01.1080p.BluRay.x264-GRP   | 5       |
    When the hunter decides
    Then it chooses "Battlestar.Galactica.1978.S01E01.1080p.BluRay.x264-GRP"

  @C11 @passing @SKADI-T-0439
  Scenario: a release far above the quality's size limit is rejected (Radarr quality definition sizes)
    Given a wanted movie "Heat" (1995) with the standard profile
    And a candidate "Heat.1995.720p.HDTV.x264-GRP" of 250000000000 bytes
    When the hunter decides
    Then no release is chosen

  @C11 @passing @SKADI-T-0432
  Scenario: a bare "WEB" source tag classifies as WEBDL (Sonarr/Radarr parity)
    Given a wanted episode "Lioness" S03E02 with the standard profile
    And a candidate "Lioness.S03E02.1080p.WEB.h264-GRP" with 50 seeders
    When the hunter decides
    Then it chooses "Lioness.S03E02.1080p.WEB.h264-GRP"
    And "Lioness.S03E02.1080p.WEB.h264-GRP" is explained as accepted at "WEBDL-1080p"

  @C11 @passing @SKADI-T-0432
  Scenario: a resolution-only release falls back to HDTV instead of being unrecognised (Sonarr parity)
    Given a wanted anime episode "Frieren" S02E10 with absolute number 38
    And a candidate "[SubsPlease] Frieren - 38 (1080p) [ABCD1234]" with 50 seeders
    When the hunter decides
    Then it chooses "[SubsPlease] Frieren - 38 (1080p) [ABCD1234]"
    And "[SubsPlease] Frieren - 38 (1080p) [ABCD1234]" is explained as accepted at "HDTV-1080p"

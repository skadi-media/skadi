Feature: Decision engine — profile, upgrade axes and custom formats
  Candidates that pass the identity gates are classified against the built-in
  quality definitions, gated by the profile (allowed list, cutoff, upgrade
  flag, custom-format Required/Ignored modes and score floor) and ranked by
  quality rank, then format score, then seeders, then age.

  @C11 @passing
  Scenario: the highest allowed quality wins regardless of seeders
    Given a wanted movie "Heat" (1995) with the standard profile
    And the candidate releases:
      | title                               | seeders |
      | Heat.1995.720p.HDTV.x264-GRP        | 900     |
      | Heat.1995.1080p.WEB-DL.x264-GRP     | 300     |
      | Heat.1995.1080p.BluRay.x264-GRP     | 3       |
    When the hunter decides
    Then it chooses "Heat.1995.1080p.BluRay.x264-GRP"
    And "Heat.1995.1080p.BluRay.x264-GRP" is explained as accepted at "Bluray-1080p"
    And the decision for "Heat.1995.1080p.BluRay.x264-GRP" is "Accept"

  @C11 @passing
  Scenario: a quality outside the profile is rejected
    Given a wanted movie "Heat" (1995) allowing only "WEBDL-1080p"
    And a candidate "Heat.1995.2160p.BluRay.x265-GRP" with 50 seeders
    When the hunter decides
    Then no release is chosen
    And "Heat.1995.2160p.BluRay.x265-GRP" is explained as rejected because "not allowed by the profile"
    And the tally rejected 1 as "quality"

  @C11 @passing
  Scenario: an unrecognised quality is rejected
    Given a wanted movie "Heat" (1995) with the standard profile
    And a candidate "Heat.1995.x264-GRP" with 50 seeders
    When the hunter decides
    Then no release is chosen
    And "Heat.1995.x264-GRP" is explained as rejected because "unrecognized quality"

  @C11 @passing
  Scenario: an upgrade run grabs only a strictly better quality
    Given a wanted movie "Heat" (1995) with the standard profile
    And the item already holds "WEBDL-1080p"
    And the candidate releases:
      | title                               | seeders |
      | Heat.1995.720p.HDTV.x264-GRP        | 900     |
      | Heat.1995.1080p.WEB-DL.x264-OTHER   | 900     |
      | Heat.1995.1080p.BluRay.x264-GRP     | 3       |
    When the hunter decides
    Then it chooses "Heat.1995.1080p.BluRay.x264-GRP"
    And the decision for "Heat.1995.1080p.BluRay.x264-GRP" is "Upgrade"
    And "Heat.1995.1080p.WEB-DL.x264-OTHER" is explained as rejected because "not an upgrade"
    And the tally rejected 2 as "quality"

  @C11 @passing
  Scenario: an item at the cutoff is not upgraded further
    Given a wanted movie "Heat" (1995) with the standard profile
    And the item already holds "Bluray-1080p"
    And a candidate "Heat.1995.2160p.BluRay.x265-GRP" with 50 seeders
    When the hunter decides
    Then no release is chosen
    And the decision for "Heat.1995.2160p.BluRay.x265-GRP" is "MeetsCutoff"

  @C11 @passing
  Scenario: a profile with upgrades disabled never upgrades
    Given a wanted movie "Heat" (1995) with the standard profile
    And the profile does not allow upgrades
    And the item already holds "HDTV-720p"
    And a candidate "Heat.1995.1080p.BluRay.x264-GRP" with 50 seeders
    When the hunter decides
    Then no release is chosen
    And the decision for "Heat.1995.1080p.BluRay.x264-GRP" is "Reject"

  @C11 @passing
  Scenario: a same-quality release with a higher format score is an upgrade (SKADI-T-0186)
    Given a wanted movie "Heat" (1995) with the standard profile
    And a custom format "Repack" matching title regex "\bREPACK\b" scoring 10
    And the item already holds "WEBDL-1080p" with format score 0
    And the candidate releases:
      | title                                     | seeders |
      | Heat.1995.1080p.WEB-DL.x264-GRP           | 900     |
      | Heat.1995.REPACK.1080p.WEB-DL.x264-GRP    | 3       |
    When the hunter decides
    Then it chooses "Heat.1995.REPACK.1080p.WEB-DL.x264-GRP"
    And the decision for "Heat.1995.REPACK.1080p.WEB-DL.x264-GRP" is "Upgrade"
    And "Heat.1995.REPACK.1080p.WEB-DL.x264-GRP" has format score 10

  @C11 @passing
  Scenario: a required custom format must be present
    Given a wanted movie "Heat" (1995) with the standard profile
    And a required custom format "HEVC" matching title regex "x265|HEVC"
    And the candidate releases:
      | title                               | seeders |
      | Heat.1995.1080p.BluRay.x264-GRP     | 900     |
      | Heat.1995.1080p.WEB-DL.x265-GRP     | 3       |
    When the hunter decides
    Then it chooses "Heat.1995.1080p.WEB-DL.x265-GRP"
    And "Heat.1995.1080p.BluRay.x264-GRP" is explained as rejected because "missing required custom format"

  @C11 @passing
  Scenario: an ignored custom format vetoes a release
    Given a wanted movie "Heat" (1995) with the standard profile
    And an ignored custom format "EVO" matching title regex "-EVO$"
    And a candidate "Heat.1995.1080p.BluRay.x264-EVO" with 900 seeders
    And a candidate "Heat.1995.720p.HDTV.x264-GRP" with 3 seeders
    When the hunter decides
    Then it chooses "Heat.1995.720p.HDTV.x264-GRP"
    And "Heat.1995.1080p.BluRay.x264-EVO" is explained as rejected because "contains ignored custom format"

  @C11 @passing
  Scenario: the minimum format score is a floor
    Given a wanted movie "Heat" (1995) with the standard profile
    And a custom format "HEVC" matching title regex "x265" scoring 10
    And the profile's minimum format score is 10
    And the candidate releases:
      | title                               | seeders |
      | Heat.1995.1080p.BluRay.x264-GRP     | 900     |
      | Heat.1995.1080p.BluRay.x265-GRP     | 3       |
    When the hunter decides
    Then it chooses "Heat.1995.1080p.BluRay.x265-GRP"
    And "Heat.1995.1080p.BluRay.x264-GRP" is explained as rejected because "below the floor"

  @C11 @passing
  Scenario: at equal quality the format score outranks seeders
    Given a wanted movie "Heat" (1995) with the standard profile
    And a custom format "WEB-DL" matching source "WEB-DL" scoring 5
    And the candidate releases:
      | title                               | seeders |
      | Heat.1995.1080p.WEBRip.x264-GRP     | 900     |
      | Heat.1995.1080p.WEB-DL.x264-GRP     | 3       |
    When the hunter decides
    Then it chooses "Heat.1995.1080p.WEB-DL.x264-GRP"
    And "Heat.1995.1080p.WEB-DL.x264-GRP" has format score 5

  @C11 @passing
  Scenario: at equal quality and score, seeders then freshness break the tie
    Given a wanted movie "Heat" (1995) with the standard profile
    And the candidate releases:
      | title                                | seeders | published_hours_ago |
      | Heat.1995.1080p.BluRay.x264-OLD      | 50      | 2000                |
      | Heat.1995.1080p.BluRay.x264-FRESH    | 50      | 3                   |
      | Heat.1995.1080p.BluRay.x264-WEAK     | 5       | 1                   |
    When the hunter decides
    Then it chooses "Heat.1995.1080p.BluRay.x264-FRESH"

  @C11 @passing
  Scenario: a movie release with a verified year outranks a yearless one
    Given a wanted movie "Heat" (1995) with the standard profile
    And the candidate releases:
      | title                               | seeders |
      | Heat.1080p.BluRay.x264-GRP          | 900     |
      | Heat.1995.1080p.BluRay.x264-GRP     | 3       |
    When the hunter decides
    Then it chooses "Heat.1995.1080p.BluRay.x264-GRP"

  @C11 @passing
  Scenario: the tally names the busiest gate first
    Given a wanted movie "Heat" (1995) with the standard profile
    And the candidate releases:
      | title                                  | seeders |
      | Blade.Runner.1982.1080p.BluRay-GRP     | 1       |
      | Inception.2010.1080p.BluRay-GRP        | 1       |
      | Heat.1995.x264-GRP                     | 1       |
    When the hunter decides
    Then no release is chosen
    And the tally summary is "2 relevance, 1 quality"

  @C11 @passing
  Scenario: the chosen release's title relevance is recorded for the trace
    Given a wanted movie "Heat" (1995) with the standard profile
    And a candidate "Heat.1995.1080p.BluRay.x264-GRP" with 3 seeders
    When the hunter decides
    Then it chooses "Heat.1995.1080p.BluRay.x264-GRP"
    And the chosen release's title relevance is at least 1.0

  @C11 @passing
  Scenario: audiobooks are gated on author identity and prefer a series pack
    Given a wanted audiobook "Storm Front" by "Jim Butcher"
    And the audiobook is book 1 of the series "The Dresden Files"
    And the candidate releases:
      | title                                                        | seeders |
      | Storm Front - Evanescence Live [MP3 320kbps]                  | 900     |
      | Jim Butcher - Storm Front (The Dresden Files #1) MP3 64kbps  | 10      |
      | Jim Butcher - The Dresden Files Books 1-8 M4B 128kbps        | 2       |
    When the hunter decides
    Then it chooses "Jim Butcher - The Dresden Files Books 1-8 M4B 128kbps"
    And the tally rejected 1 as "identity"

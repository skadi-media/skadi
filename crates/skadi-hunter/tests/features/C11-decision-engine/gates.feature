Feature: Decision gates — identity, category, size, seeders, blocklist
  `pipeline::decide` rejects a candidate before ranking when it is not the
  wanted item (title relevance, TV episode identity, movie year / sequel
  marker), is tagged in the wrong Torznab group, is a gross size outlier,
  is under-seeded, or is on the blocklist. Each gate tallies its victims.

  @C11 @passing
  Scenario: unrelated titles are rejected by the relevance gate
    Given a wanted movie "The Matrix" (1999) with the standard profile
    And the candidate releases:
      | title                                   | seeders |
      | The.Matrix.1999.1080p.BluRay.x264-GRP   | 10      |
      | Blade.Runner.2049.1080p.BluRay.x264-XYZ | 100     |
      | Inception.2010.1080p.BluRay.x264-ABC    | 200     |
    When the hunter decides
    Then it chooses "The.Matrix.1999.1080p.BluRay.x264-GRP"
    And the tally rejected 2 as "relevance"
    And the tally considered 3 candidates

  @C11 @passing
  Scenario: nothing relevant means no suitable release
    Given a wanted movie "The Matrix" (1999) with the standard profile
    And a candidate "Blade.Runner.2049.1080p.BluRay.x264-XYZ" with 100 seeders
    When the hunter decides
    Then no release is chosen
    And the tally rejected 1 as "relevance"

  @C11 @passing
  Scenario: a search for S03E02 never accepts S02E06 (SKADI-T-0386)
    Given a wanted episode "Lioness" S03E02 with the standard profile
    And the candidate releases:
      | title                                      | seeders |
      | Lioness.2023.S02E06.1080p.WEB-DL.h264-GRP     | 500     |
      | Lioness.S03E02.1080p.WEB-DL.h264-GRP          | 5       |
      | Lioness.S03E03.1080p.WEB-DL.h264-GRP          | 50      |
    When the hunter decides
    Then it chooses "Lioness.S03E02.1080p.WEB-DL.h264-GRP"
    And the tally rejected 2 as "episode"

  @C11 @passing
  Scenario: a same-named feature film is rejected for a show (SKADI-T-0386)
    Given a wanted episode "Defiance" S03E01 with the standard profile
    And the candidate releases:
      | title                                     | seeders |
      | Defiance.2013.1080p.BluRay.x264-GRP       | 900     |
      | Defiance.2013.S03E01.720p.HDTV.x264-GRP   | 3       |
    When the hunter decides
    Then it chooses "Defiance.2013.S03E01.720p.HDTV.x264-GRP"
    And the tally rejected 1 as "episode"

  @C11 @passing
  Scenario: a yearless episode name still classifies to a real quality (SKADI-T-0386)
    Given a wanted episode "Lioness" S03E02 with the standard profile
    And a candidate "Lioness.S03E02.1080p.WEB-DL.h264-GRP" with 5 seeders
    When the hunter decides
    Then it chooses "Lioness.S03E02.1080p.WEB-DL.h264-GRP"
    And "Lioness.S03E02.1080p.WEB-DL.h264-GRP" is explained as accepted at "WEBDL-1080p"

  @C11 @passing @SKADI-T-0402
  Scenario: neither a wrong episode nor a season pack satisfies a single-episode search
    Given a wanted episode "House of the Dragon" S03E02 with the standard profile
    And the candidate releases:
      | title                                                  | seeders |
      | House.of.the.Dragon.S02E06.1080p.WEB-DL.x264-GRP       | 800     |
      | House.of.the.Dragon.S03.1080p.AMZN.WEB-DL.x265-GRP     | 40      |
    When the hunter decides
    Then no release is chosen
    And the tally rejected 2 as "episode"

  @C11 @passing @SKADI-T-0402
  Scenario: the single wanted episode beats a season pack (Sonarr parity)
    Given a wanted episode "House of the Dragon" S03E02 with the standard profile
    And the candidate releases:
      | title                                                  | seeders |
      | House.of.the.Dragon.S03E02.1080p.WEB-DL.x264-GRP       | 800     |
      | House.of.the.Dragon.S03.1080p.AMZN.WEB-DL.x265-GRP     | 40      |
    When the hunter decides
    Then it chooses "House.of.the.Dragon.S03E02.1080p.WEB-DL.x264-GRP"

  @C11 @passing
  Scenario: a season-pack search accepts only packs for that season
    Given a wanted season pack "Archer" S03 with the standard profile
    And the candidate releases:
      | title                                   | seeders |
      | Archer.2009.S03E04.1080p.WEB-DL.h264-GRP   | 80      |
      | Archer.2009.S02.1080p.WEB-DL.h264-GRP      | 90      |
      | Archer.2009.S03.1080p.WEB-DL.h264-GRP      | 20      |
    When the hunter decides
    Then it chooses "Archer.2009.S03.1080p.WEB-DL.h264-GRP"
    And the tally rejected 2 as "episode"

  @C11 @passing
  Scenario: specials must match S00Exx exactly and never take a pack shortcut
    Given a wanted episode "Archer" S00E01 with the standard profile
    And the candidate releases:
      | title                                                 | seeders |
      | Archer.2009.S02E06.1080p.WEB-DL.h264-GRP                 | 500     |
      | Archer.2009.S00.Heart.of.Archness.1080p.WEB-DL.h264-GRP  | 200     |
      | Archer.Complete.Series.1080p.BluRay.x264-GRP          | 100     |
      | Archer.2009.S00E01.Archersaurus.1080p.WEB-DL.h264-GRP    | 2       |
    When the hunter decides
    Then it chooses "Archer.2009.S00E01.Archersaurus.1080p.WEB-DL.h264-GRP"
    And the tally rejected 3 as "episode"

  @C11 @passing @SKADI-T-0402
  Scenario: a complete-series pack is not grabbed for a single wanted episode
    Given a wanted episode "Angel" S02E03 with the standard profile
    And a candidate "Angel.Complete.Series.1080p.BluRay.x264-GRP" with 10 seeders
    When the hunter decides
    Then no release is chosen
    And the tally rejected 1 as "episode"

  @C11 @passing @SKADI-T-0402
  Scenario: a complete-series pack does cover a season-scoped search
    Given a wanted season pack "Angel" S02 with the standard profile
    And a candidate "Angel.Complete.Series.1080p.BluRay.x264-GRP" with 10 seeders
    When the hunter decides
    Then it chooses "Angel.Complete.Series.1080p.BluRay.x264-GRP"

  @C11 @passing
  Scenario: an absolute-numbered anime release is verified against the wanted number
    Given a wanted anime episode "Frieren" S02E10 with absolute number 38
    And the candidate releases:
      | title                                       | seeders |
      | [SubsPlease] Frieren - 39 (1080p WEB-DL) [ABCD1234] | 900     |
      | [SubsPlease] Frieren - 38 (1080p WEB-DL) [ABCD1234] | 5       |
    When the hunter decides
    Then it chooses "[SubsPlease] Frieren - 38 (1080p WEB-DL) [ABCD1234]"
    And the tally rejected 1 as "episode"

  @C11 @passing
  Scenario: a date-keyed daily release is verified against the wanted air date
    Given a wanted daily episode "The Daily Show" S29E40 aired "2024-03-01"
    And the candidate releases:
      | title                                          | seeders |
      | The.Daily.Show.2024.03.02.1080p.WEB-DL.h264-GRP   | 900     |
      | The.Daily.Show.2024.03.01.1080p.WEB-DL.h264-GRP   | 5       |
    When the hunter decides
    Then it chooses "The.Daily.Show.2024.03.01.1080p.WEB-DL.h264-GRP"
    And the tally rejected 1 as "episode"

  @C11 @passing
  Scenario: the movie year gate rejects the original and the sequel (SKADI-T-0387)
    Given a wanted movie "101 Dalmatians" (1996) with the standard profile
    And the candidate releases:
      | title                                          | seeders |
      | 101.Dalmatians.1961.1080p.BluRay.x264-GRP      | 300     |
      | 101.Dalmatians.II.2003.1080p.BluRay.x264-GRP   | 200     |
      | 101.Dalmatians.1080p.BluRay.x264-GRP           | 100     |
      | 101.Dalmatians.1997.1080p.BluRay.x264-GRP      | 1       |
    When the hunter decides
    Then it chooses "101.Dalmatians.1997.1080p.BluRay.x264-GRP"
    And the tally rejected 2 as "year"

  @C11 @passing
  Scenario: a yearless sequel marker the wanted title lacks is rejected (SKADI-T-0387)
    Given a wanted movie "101 Dalmatians" (1996) with the standard profile
    And a candidate "101.Dalmatians.II.1080p.BluRay.x264-GRP" with 50 seeders
    When the hunter decides
    Then no release is chosen
    And the tally rejected 1 as "relevance"

  @C11 @passing
  Scenario: "Les Insurgés (2008)" is not accepted for Insurgent (2015) (SKADI-T-0401 evidence)
    Given a wanted movie "Insurgent" (2015) with the standard profile
    And a candidate "Les.Insurgés.2008.MULTi.1080p.BluRay.x264-LRL" with 50 seeders
    When the hunter decides
    Then no release is chosen
    And the tally rejected 1 as "relevance"

  @C11 @passing
  Scenario: off-category releases are rejected, untagged ones pass
    Given a wanted movie "Wasteland" (2020) with the standard profile
    And the candidate releases:
      | title                                     | seeders | category |
      | Wasteland.2020.2160p.BluRay.x265-HOODLUM | 500     | 4050     |
      | Wasteland.2020.1080p.BluRay.x264-GRP     | 5       | 2040     |
    And a candidate "Wasteland.2020.720p.HDTV.x264-UNTAGGED" with 900 seeders
    When the hunter decides
    Then it chooses "Wasteland.2020.1080p.BluRay.x264-GRP"
    And the tally rejected 1 as "category"

  @C11 @passing
  Scenario: a gross size outlier is not our media
    Given a wanted movie "Heat" (1995) with the standard profile
    And a candidate "Heat.1995.1080p.BluRay.x264-GRP" of 20480 bytes
    When the hunter decides
    Then no release is chosen
    And the tally rejected 1 as "size"

  @C11 @passing
  Scenario: torrents under the seeder floor are skipped, usenet never is
    Given a wanted movie "Heat" (1995) with the standard profile
    And the profile requires at least 5 seeders
    And a candidate "Heat.1995.2160p.BluRay.x265-DEAD" with 2 seeders
    And a usenet candidate "Heat.1995.1080p.BluRay.x264-NZB"
    When the hunter decides
    Then it chooses "Heat.1995.1080p.BluRay.x264-NZB"
    And the tally rejected 1 as "seeders"
    And "Heat.1995.2160p.BluRay.x265-DEAD" is explained as rejected because "too few seeders"

  @C11 @passing
  Scenario: a blocklisted release is skipped for the next best
    Given a wanted movie "Heat" (1995) with the standard profile
    And the candidate releases:
      | title                               | seeders |
      | Heat.1995.2160p.BluRay.x265-DEAD    | 900     |
      | Heat.1995.1080p.BluRay.x264-GRP     | 9       |
    And "Heat.1995.2160p.BluRay.x265-DEAD" is on the blocklist
    When the hunter decides
    Then it chooses "Heat.1995.1080p.BluRay.x264-GRP"
    And the tally rejected 1 as "blocklisted"
    And "Heat.1995.2160p.BluRay.x265-DEAD" is explained as rejected because "blocklisted"

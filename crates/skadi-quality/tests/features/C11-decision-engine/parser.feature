Feature: Release-title parser — clean-room movie, TV and audiobook parsing
  `parse` splits a movie title from its tag soup on the year (falling back to
  the first quality tag), `parse_tv` on the episode marker, and
  `parse_audiobook` on the `Author - Title (Year) [Series ##] {Format}` shape.
  The embedded corpora are the regression contract (REQ-DECIDE.1/.2/.13).

  @C11 @passing
  Scenario: the movie seed corpus is the parser's contract
    When the movie corpus is replayed
    Then every one of its at least 30 entries parses as expected

  @C11 @passing
  Scenario: the audiobook seed corpus is the audiobook parser's contract
    When the audiobook corpus is replayed
    Then every one of its at least 8 entries parses as expected

  @C11 @passing
  Scenario: a scene-style BluRay release decomposes fully
    Given the release title "The.Matrix.1999.1080p.BluRay.x264-AMIABLE"
    When it is parsed as a movie
    Then the title is "The Matrix"
    And the year is 1999
    And the resolution is "1080p"
    And the source is "BluRay"
    And the codec is "x264"
    And the group is "AMIABLE"

  @C11 @passing
  Scenario: container extensions and repost brackets are stripped before the group
    Given the release title "Another.Film.2014.720p.HDTV.x264-2HD[eztv]-[rarbg.com].mkv"
    When it is parsed as a movie
    Then the title is "Another Film"
    And the group is "2HD"
    And the source is "HDTV"

  @C11 @passing
  Scenario: editions, modifiers and languages are extracted in canonical form
    Given the release title "Kingdom.of.Heaven.2005.Directors.Cut.REPACK.FRENCH.1080p.BluRay.x264-GRP"
    When it is parsed as a movie
    Then the edition is "Director's Cut"
    And the modifiers are "Repack"
    And the languages are "French"

  @C11 @passing
  Scenario: a remux source implies the Remux modifier and HDR tags do not leak
    Given the release title "Gladiator.2000.2160p.UHD.BDRemux.HDR.DV.HEVC.TrueHD.7.1-GRP"
    When it is parsed as a movie
    Then the source is "BDRemux"
    And the modifiers are "Remux"
    And the codec is "HEVC"

  @C11 @passing
  Scenario: a yearless movie name splits on the first quality tag (SKADI-T-0387)
    Given the release title "Heat.1080p.BluRay.x264-GRP"
    When it is parsed as a movie
    Then the title is "Heat"
    And there is no year
    And the resolution is "1080p"

  @C11 @passing
  Scenario: an untagged release does not assume English
    Given the release title "Plain.Movie.2019.1080p.BluRay.x264-GRP"
    When it is parsed as a movie
    Then the languages are ""

  @C11 @passing
  Scenario: a table of real-world movie shapes
    Then a corpus of titles parses as expected:
      | title                                               | year | resolution | source  |
      | Dune.Part.Two.2024.2160p.WEB-DL.DDP5.1.Atmos.H.265-FLUX | 2024 | 2160p  | WEB-DL  |
      | Oppenheimer.2023.1080p.WEBRip.x265-RARBG            | 2023 | 1080p      | WEBRip  |
      | Old.Movie.1960.DVDRip.x264-TEAM                     | 1960 |            | DVDRip  |
      | Heat 1995 1080p BluRay x264 GRP                     | 1995 | 1080p      | BluRay  |

  @C11 @passing
  Scenario: a single episode with a year in the show name
    Given the release title "Lioness.2023.S02E06.1080p.WEB-DL.h264-GRP"
    When it is parsed as a TV episode
    Then the title is "Lioness 2023"
    And the year is 2023
    And it is season 2 episode 6
    And the resolution is "1080p"
    And the source is "WEB-DL"

  @C11 @passing
  Scenario: a yearless episode still classifies (the SKADI-T-0386 root cause)
    Given the release title "Lioness.S03E02.1080p.WEB-DL.h264-GRP"
    When it is parsed as a TV episode
    Then the title is "Lioness"
    And it is season 3 episode 2
    And the resolution is "1080p"
    And the source is "WEB-DL"
    And the group is "GRP"

  @C11 @passing
  Scenario: an ascending episode range is expanded, a descending one is not
    Given the release title "Show.S01E05-E07.1080p.WEB-DL.x264-GRP"
    When it is parsed as a TV episode
    Then it is season 1 episodes "5,6,7"
    Given the release title "Show.S01E36-1.28.1080p.WEB-DL.x264-GRP"
    When it is parsed as a TV episode
    Then it is season 1 episode 36

  @C11 @passing
  Scenario: alternate 1x05 numbering and underscores are understood
    Given the release title "show_name_-_1x05_-_episode_title_720p_hdtv"
    When it is parsed as a TV episode
    Then it is season 1 episode 5
    And the title is "show name"

  @C11 @passing @SKADI-T-0434
  Scenario: quality tags in an underscore-delimited library name are parsed
    Given the release title "show_name_-_1x05_-_episode_title_720p_hdtv"
    When it is parsed as a TV episode
    Then it is season 1 episode 5
    And the resolution is "720p"
    And the source is "hdtv"

  @C11 @passing
  Scenario: whole-season packs in both spellings
    Given the release title "House.of.the.Dragon.S03.1080p.AMZN.WEB-DL.x265-GRP"
    When it is parsed as a TV episode
    Then it is a full season 3 pack
    Given the release title "Archer Season 3 Complete 1080p BluRay x264-GRP"
    When it is parsed as a TV episode
    Then it is a full season 3 pack

  @C11 @passing
  Scenario: anime absolute numbering with a leading group tag
    Given the release title "[SubsPlease] Frieren - 38 (1080p) [ABCD1234].mkv"
    When it is parsed as a TV episode
    Then the title is "Frieren"
    And it is absolute episode 38
    And the resolution is "1080p"

  @C11 @passing
  Scenario: daily shows are keyed by air date
    Given the release title "The.Daily.Show.2024.03.01.1080p.WEB-DL.h264-GRP"
    When it is parsed as a TV episode
    Then the title is "The Daily Show"
    And it aired on "2024-03-01"

  @C11 @passing
  Scenario: a feature film carries no TV markers
    Given the release title "Defiance.2013.1080p.BluRay.x264-GRP"
    When it is parsed as a TV episode
    Then it carries no TV markers
    And the year is 2013

  @C11 @passing
  Scenario: an audiobook release yields author, title, format, bitrate and series
    Given the release title "Brandon Sanderson - The Way of Kings [Stormlight Archive 01] (2010) {MP3 64kbps Unabridged}"
    When it is parsed as an audiobook
    Then the author is "Brandon Sanderson"
    And the title is "The Way of Kings"
    And the year is 2010
    And the audio format is "MP3" at 64 kbps
    And the series is "Stormlight Archive" position "1"
    And it is unabridged

  @C11 @passing
  Scenario: an abridged signal and a multi-book pack are detected
    Given the release title "Stephen King - The Stand {Abridged} 32kbps MP3"
    When it is parsed as an audiobook
    Then it is abridged
    Given the release title "Jim Butcher - The Dresden Files Books 1-8 M4B 128kbps"
    When it is parsed as an audiobook
    Then it is a multi-book pack

  @C11 @passing
  Scenario: the taxonomy folds aliases case-insensitively and orders resolutions
    Then the token "UHD" maps to resolution "R2160p"
    And the token "4k" maps to resolution "R2160p"
    And the token "1080P" maps to resolution "R1080p"
    And the token "web-dl" maps to source "WebDl"
    And the token "WEBRip" maps to source "WebRip"
    And the token "BDRemux" maps to source "Bluray"
    And the token "BRRip" maps to source "Bluray"
    And resolutions order SD < 480p < 576p < 720p < 1080p < 2160p

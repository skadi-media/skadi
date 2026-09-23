Feature: Quality recorded on adoption vs regular import (C20, SKADI-T-0399)
  A regular import probes the placed file and reconciles the release-title parse
  with the real resolution. Library adoption (movies/tv/audiobooks `commit_*`)
  parses the file name only and falls back to the lowest built-in tier when
  nothing parses — which the upgrade sweep reads as "below cutoff".

  @C20 @bug @SKADI-T-0399
  Scenario: an adopted library file with no quality tokens in its name is recorded as the lowest real tier
    Given an existing library file named "The Matrix (1999).mkv"
    When its quality is derived from the file name the way adoption does
    Then no quality can be parsed from the name
    And the lowest built-in definition the adoption fallback records is "SDTV"
    And an explicit Unknown quality tier exists for unassessed files

  @C20 @passing
  Scenario: a release name with resolution and source parses to a real tier on adoption
    Given an existing library file named "The.Matrix.1999.1080p.BluRay.x264-GRP.mkv"
    When its quality is derived from the file name the way adoption does
    Then the quality is "Bluray-1080p"

  @C20 @passing
  Scenario: a regular import corrects a mislabelled 2160p release to the probed 1080p
    Given an existing library file named "The.Matrix.1999.2160p.BluRay.x264-GRP.mkv"
    When the file is probed at 1920x1080 and reconciled with the name
    Then the quality is "Bluray-1080p"

  @C20 @gap
  Scenario: a probed resolution alone can back-fill an adopted file's quality
    Given an existing library file named "The Matrix (1999).mkv"
    When the file is probed at 1920x1080 and reconciled with the name
    Then the probed resolution is reflected in the parse
    And a quality is derived from the probe alone

  @C20 @passing @SKADI-T-0412
  Scenario: a probed standard-definition DVD rip maps to the DVD tier instead of no quality at all
    Given an existing library file named "Movie.2004.DVDRip.XviD-GRP.avi"
    When the file is probed at 720x480 and reconciled with the name
    Then the probed resolution is reflected in the parse
    And the quality is "DVD"

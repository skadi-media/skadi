Feature: Import preview and the manual-import scan (C20)
  `preview` is the dry-run behind the manual-import UI: the same matching, floor,
  collision and space decisions with no files touched. `scan_files` collects an
  operator-supplied path. Mirrors Sonarr/Radarr Manual Import.

  @C20 @passing
  Scenario: a preview plans the placements and touches nothing
    Given a completed download containing "S01E01.mkv"
    And the completed download also contains "S01E02.mkv"
    And the domain maps "S01E01.mkv" to library path "television/show/Season_01/S01E01.mkv"
    And the domain maps "S01E02.mkv" to library path "television/show/Season_01/S01E02.mkv"
    When the import is previewed
    Then the plan would place "television/show/Season_01/S01E01.mkv"
    And the plan would place "television/show/Season_01/S01E02.mkv"
    And the library root contains no files

  @C20 @passing
  Scenario: a preview applies the same sample, size and match floors as an import
    Given a completed download containing "Movie.2020-sample.mkv"
    And the completed download also contains "tiny.mkv" of 5 bytes
    And the completed download also contains "readme.txt"
    And the importer rejects files smaller than 10 bytes
    And the domain would place "tiny.mkv" at library path "movie/tiny.mkv" if asked
    When the import is previewed
    Then the plan imports nothing
    And the plan would reject source "Movie.2020-sample.mkv" with reason containing "sample"
    And the plan would reject source "tiny.mkv" with reason containing "below size floor"
    And the plan would reject source "readme.txt" with reason containing "no matching acquirable"

  @C20 @passing @SKADI-T-0428
  Scenario: a preview reports an occupied skip destination the same way the import does
    # The preview's whole job is to predict the import. Reporting the same case as
    # a rejection here and as already-present there made it show a failure for a
    # library that is simply already correct.
    Given a completed download containing "Movie.2020.mkv"
    And the library already holds "movie/Movie.mkv" with contents "here"
    And the domain maps "Movie.2020.mkv" to library path "movie/Movie.mkv"
    When the import is previewed
    Then the plan reports library path "movie/Movie.mkv" as already present
    When the download is imported
    Then 1 file is reported already present

  @C20 @passing
  Scenario: a preview refuses a placement the free-space reserve cannot hold
    Given a completed download containing "Movie.2020.mkv"
    And the importer keeps an unmeetable free-space reserve
    And the domain maps "Movie.2020.mkv" to library path "movie/Movie.mkv"
    When the import is previewed
    Then the plan would reject library path "movie/Movie.mkv" with reason containing "insufficient free space"

  @C20 @passing
  Scenario: scanning a single file yields that file
    Given a download folder containing "Movie.mkv"
    When the download file "Movie.mkv" is scanned for manual import
    Then the scan yields "Movie.mkv" in that order

  @C20 @passing
  Scenario: scanning a folder walks it recursively in sorted order
    Given a download folder containing "b/S01E02.mkv, a/S01E01.mkv, readme.txt"
    When the download folder is scanned for manual import
    Then the scan yields "a/S01E01.mkv, b/S01E02.mkv, readme.txt" in that order

  @C20 @passing
  Scenario: an unreadable subfolder is skipped rather than failing the scan
    Given a download folder containing "ok/S01E01.mkv"
    And the download subfolder "locked" is unreadable
    When the download folder is scanned for manual import
    Then the scan yields "ok/S01E01.mkv" in that order

  @C20 @passing
  Scenario: scanning a missing path is an error
    When a missing path is scanned for manual import
    Then the scan fails

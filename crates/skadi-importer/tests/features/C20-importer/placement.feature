Feature: Placing completed downloads into the library (C20)
  The mechanical importer hardlinks each matched file into the library (copy
  fallback across filesystems), creates parent folders, never touches the source,
  and isolates per-file failures. Mirrors Sonarr/Radarr Completed Download
  Handling with "Use Hardlinks instead of Copy".

  @C20 @passing
  Scenario: a completed download is hardlinked into the library and the source stays seedable
    Given a completed download containing "Movie.2020.1080p.BluRay.x264-G.mkv"
    And the domain maps "Movie.2020.1080p.BluRay.x264-G.mkv" to library path "movie/Movie_(2020)/Movie_(2020).mkv" as acquirable "edition-1"
    When the download is imported
    Then 1 file is imported
    And the import for acquirable "edition-1" landed at library path "movie/Movie_(2020)/Movie_(2020).mkv"
    And library path "movie/Movie_(2020)/Movie_(2020).mkv" is a hardlink of source "Movie.2020.1080p.BluRay.x264-G.mkv"
    And source "Movie.2020.1080p.BluRay.x264-G.mkv" still exists
    And nothing is rejected
    And nothing failed

  @C20 @passing
  Scenario: parent folders are created for a deeply nested destination
    Given a completed download containing "book.m4b"
    And the domain maps "book.m4b" to library path "audiobook/stephen-king/the-dark-tower/1 - the-gunslinger {asin-B002UZMLXM}/the-gunslinger.m4b"
    When the download is imported
    Then 1 file is imported
    And library path "audiobook/stephen-king/the-dark-tower/1 - the-gunslinger {asin-B002UZMLXM}/the-gunslinger.m4b" exists

  @C20 @passing
  Scenario: a title with curly quotes lands at a unicode path
    Given a completed download containing "Salems.Lot.2024.1080p.WEB.h264.mkv"
    And the domain maps "Salems.Lot.2024.1080p.WEB.h264.mkv" to library path "movie/‘Salem’s_Lot_(2024)/‘Salem’s_Lot_(2024).mkv"
    When the download is imported
    Then 1 file is imported
    And library path "movie/‘Salem’s_Lot_(2024)/‘Salem’s_Lot_(2024).mkv" has the same bytes as source "Salems.Lot.2024.1080p.WEB.h264.mkv"

  @C20 @passing
  Scenario: one source file can satisfy two acquirables
    Given a completed download containing "Movie.2020.mkv"
    And the domain maps "Movie.2020.mkv" to library path "movie/a/Movie.mkv" as acquirable "theatrical"
    And the domain maps "Movie.2020.mkv" to library path "movie/b/Movie.mkv" as acquirable "directors-cut"
    When the download is imported
    Then 2 files are imported
    And the import for acquirable "theatrical" landed at library path "movie/a/Movie.mkv"
    And the import for acquirable "directors-cut" landed at library path "movie/b/Movie.mkv"

  @C20 @passing
  Scenario: when hardlinking is unavailable the file is copied atomically and the source is kept
    Given a source file "Movie.mkv" with contents "video bytes"
    When "Movie.mkv" is placed at library path "movie/Movie_(2020)/Movie_(2020).mkv" without hardlinks
    Then the placement succeeds
    And library path "movie/Movie_(2020)/Movie_(2020).mkv" has contents "video bytes"
    And library path "movie/Movie_(2020)/Movie_(2020).mkv" is not a hardlink of source "Movie.mkv"
    And source "Movie.mkv" still exists
    And no staging or partial files are left beside "movie/Movie_(2020)/Movie_(2020).mkv"

  @C20 @passing
  Scenario: an interrupted copy never leaves a partial file at the destination
    Given a source file "Movie.mkv" with contents "video bytes"
    And a partial file is left at "movie/Movie.mkv" from an interrupted copy
    When "Movie.mkv" is placed at library path "movie/Movie.mkv" without hardlinks
    Then the placement succeeds
    And library path "movie/Movie.mkv" has contents "video bytes"
    And no staging or partial files are left beside "movie/Movie.mkv"

  @C20 @passing
  Scenario: an unwritable destination folder fails only that file and the rest still import
    Given a completed download containing "S01E01.mkv"
    And the completed download also contains "S01E02.mkv"
    And the library directory "television/locked" is read-only
    And the domain maps "S01E01.mkv" to library path "television/locked/S01E01.mkv"
    And the domain maps "S01E02.mkv" to library path "television/open/S01E02.mkv"
    When the download is imported
    Then 1 file is imported
    And 1 file is failed
    And library path "television/locked/S01E01.mkv" is reported failed with reason containing "ermission denied"
    And library path "television/open/S01E02.mkv" exists
    And every source file appears in exactly one outcome bucket

  @C20 @passing
  Scenario: a destination name longer than the filesystem allows fails that file with a reason
    Given a completed download containing "long.mkv"
    And the domain maps "long.mkv" to library path "movie/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.mkv"
    When the download is imported
    Then nothing is imported
    And 1 file is failed
    And a failure reason contains "too long"
    And every source file appears in exactly one outcome bucket

  @C20 @passing
  Scenario: a missing source file is a per-file failure, not an aborted import
    Given a completed download containing "good.mkv"
    And the completed download lists "vanished.mkv" which is missing on disk
    And the domain maps "good.mkv" to library path "movie/good.mkv"
    And the domain maps "vanished.mkv" to library path "movie/vanished.mkv"
    When the download is imported
    Then 1 file is imported
    And 1 file is failed
    And library path "movie/vanished.mkv" does not exist

  @C20 @passing
  Scenario: generated sidecars are written next to the placed file
    Given a completed download containing "Movie.2020.mkv"
    And the domain maps "Movie.2020.mkv" to library path "movie/Movie_(2020)/Movie_(2020).mkv"
    And the domain attaches sidecar "movie/Movie_(2020)/Movie_(2020).nfo" with contents "<movie/>" to "Movie.2020.mkv"
    When the download is imported
    Then 1 file is imported
    And library path "movie/Movie_(2020)/Movie_(2020).nfo" has contents "<movie/>"

  @C20 @passing
  Scenario: a sidecar that cannot be written never fails the import
    Given a completed download containing "Movie.2020.mkv"
    And the library directory "movie/nfo-locked" is read-only
    And the domain maps "Movie.2020.mkv" to library path "movie/Movie_(2020)/Movie_(2020).mkv"
    And the domain attaches sidecar "movie/nfo-locked/Movie.nfo" with contents "<movie/>" to "Movie.2020.mkv"
    When the download is imported
    Then 1 file is imported
    And nothing failed
    And library path "movie/nfo-locked/Movie.nfo" does not exist

  @C20 @passing @SKADI-T-0419
  Scenario: the operator can switch imports from hardlink to copy (Sonarr "Use Hardlinks instead of Copy")
    Given the importer is configured to copy instead of hardlink
    And a completed download containing "Movie.2020.mkv"
    And the domain maps "Movie.2020.mkv" to library path "movie/Movie.mkv"
    When the download is imported
    Then library path "movie/Movie.mkv" is not a hardlink of source "Movie.2020.mkv"

  @C20 @passing @SKADI-T-0419
  Scenario: placed files get the operator's configured permissions (Sonarr "Set Permissions")
    Given the importer is configured to set file mode 0o640
    And a completed download containing "Movie.2020.mkv"
    And the domain maps "Movie.2020.mkv" to library path "movie/Movie.mkv"
    When the download is imported
    Then library path "movie/Movie.mkv" has the operator's configured file mode

  @C20 @passing @SKADI-T-0138
  Scenario: the default keeps the source so the torrent goes on seeding
    Given a completed download containing "Movie.2020.mkv"
    And the domain maps "Movie.2020.mkv" to library path "movie/Movie.mkv"
    When the download is imported
    Then 1 file is imported
    And the source file "Movie.2020.mkv" is still there

  @C20 @passing @SKADI-T-0138
  Scenario: move placement removes the source once the library copy is placed
    Given the operator chooses move-on-import placement
    And a completed download containing "Movie.2020.mkv"
    And the domain maps "Movie.2020.mkv" to library path "movie/Movie.mkv"
    When the download is imported
    Then 1 file is imported
    And library path "movie/Movie.mkv" has contents "payload of Movie.2020.mkv"
    And the source file "Movie.2020.mkv" is gone

  @C20 @passing @SKADI-T-0138
  Scenario: an unrecognised placement value keeps the safe default
    # A typo must never silently start deleting sources.
    Given the operator sets import placement to "hardlnk"
    And a completed download containing "Movie.2020.mkv"
    And the domain maps "Movie.2020.mkv" to library path "movie/Movie.mkv"
    When the download is imported
    Then the source file "Movie.2020.mkv" is still there

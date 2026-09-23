Feature: Destination collisions and upgrade replacement (C20)
  An occupied destination is resolved per the domain's collision policy (Skip is
  the safe default and means "already this acquirable's file" — SKADI-T-0385);
  Overwrite is an atomic stage-then-rename; an upgrade removes the superseded
  file it replaces. Mirrors Sonarr/Radarr upgrade-and-replace, minus the
  Recycling Bin.

  @C20 @passing
  Scenario: an occupied destination under the default policy is reported already present, not rejected
    Given a completed download containing "Saturation.Point.m4b"
    And the library already holds "audiobook/saturation-point/saturation-point.m4b" with contents "imported earlier"
    And the domain maps "Saturation.Point.m4b" to library path "audiobook/saturation-point/saturation-point.m4b" as acquirable "book-99c3bdc8"
    When the download is imported
    Then nothing is imported
    And nothing is rejected
    And 1 file is reported already present
    And library path "audiobook/saturation-point/saturation-point.m4b" is reported already present for acquirable "book-99c3bdc8"
    And library path "audiobook/saturation-point/saturation-point.m4b" has contents "imported earlier"

  @C20 @passing
  Scenario: a re-downloaded multi-file audiobook whose parts are all in the library resolves as present (SKADI-T-0385 regression)
    Given a completed download containing "Part 01.mp3"
    And the completed download also contains "Part 02.mp3"
    And the completed download also contains "Part 03.mp3"
    And the library already holds "audiobook/book/Part 01.mp3" with contents "p1"
    And the library already holds "audiobook/book/Part 02.mp3" with contents "p2"
    And the library already holds "audiobook/book/Part 03.mp3" with contents "p3"
    And the domain maps "Part 01.mp3" to library path "audiobook/book/Part 01.mp3" as acquirable "book-1"
    And the domain maps "Part 02.mp3" to library path "audiobook/book/Part 02.mp3" as acquirable "book-1"
    And the domain maps "Part 03.mp3" to library path "audiobook/book/Part 03.mp3" as acquirable "book-1"
    When the download is imported
    Then nothing is imported
    And nothing is rejected
    And 3 files are reported already present
    And every source file appears in exactly one outcome bucket

  @C20 @passing
  Scenario: a partially present multi-file download places the missing parts and reports the rest present
    Given a completed download containing "Part 01.mp3"
    And the completed download also contains "Part 02.mp3"
    And the library already holds "audiobook/book/Part 01.mp3" with contents "p1"
    And the domain maps "Part 01.mp3" to library path "audiobook/book/Part 01.mp3" as acquirable "book-1"
    And the domain maps "Part 02.mp3" to library path "audiobook/book/Part 02.mp3" as acquirable "book-1"
    When the download is imported
    Then 1 file is imported
    And 1 file is reported already present
    And library path "audiobook/book/Part 02.mp3" exists

  @C20 @passing
  Scenario: re-running the same import after a crash converges instead of duplicating
    Given a completed download containing "Movie.2020.mkv"
    And the domain maps "Movie.2020.mkv" to library path "movie/Movie.mkv" as acquirable "ed-1"
    When the download is imported
    And the download is imported again
    Then nothing is imported
    And 1 file is reported already present
    And library path "movie/Movie.mkv" is reported already present for acquirable "ed-1"

  @C20 @passing
  Scenario: the overwrite policy replaces the existing file atomically and reports it replaced
    Given a completed download containing "Movie.2020.2160p.mkv"
    And the library already holds "movie/Movie.mkv" with contents "old 1080p"
    And the domain maps "Movie.2020.2160p.mkv" to library path "movie/Movie.mkv" with collision policy overwrite
    When the download is imported
    Then 1 file is imported
    And library path "movie/Movie.mkv" has the same bytes as source "Movie.2020.2160p.mkv"
    And library path "movie/Movie.mkv" is listed as replaced
    And no staging or partial files are left beside "movie/Movie.mkv"

  @C20 @passing
  Scenario: the error policy fails the file and leaves the existing one untouched
    Given a completed download containing "Movie.2020.mkv"
    And the library already holds "movie/Movie.mkv" with contents "keep me"
    And the domain maps "Movie.2020.mkv" to library path "movie/Movie.mkv" with collision policy error
    When the download is imported
    Then nothing is imported
    And 1 file is failed
    And library path "movie/Movie.mkv" is reported failed with reason containing "already exists"
    And library path "movie/Movie.mkv" has contents "keep me"

  @C20 @passing
  Scenario: a failed overwrite leaves the existing file intact and cleans its staging
    Given a completed download containing "Movie.2020.mkv"
    And the completed download lists "vanished.mkv" which is missing on disk
    And the library already holds "movie/Movie.mkv" with contents "keep me"
    And the domain maps "vanished.mkv" to library path "movie/Movie.mkv" with collision policy overwrite
    When the download is imported
    Then 1 file is failed
    And library path "movie/Movie.mkv" has contents "keep me"
    And no staging or partial files are left beside "movie/Movie.mkv"

  @C20 @passing
  Scenario: a stale staging file from an interrupted replace is cleared and the replace completes
    Given a completed download containing "Movie.2020.mkv"
    And the library already holds "movie/Movie.mkv" with contents "old"
    And a stale staging file is left beside "movie/Movie.mkv" from an interrupted replace
    And the domain maps "Movie.2020.mkv" to library path "movie/Movie.mkv" with collision policy overwrite
    When the download is imported
    Then 1 file is imported
    And library path "movie/Movie.mkv" has the same bytes as source "Movie.2020.mkv"
    And no staging or partial files are left beside "movie/Movie.mkv"

  @C20 @passing
  Scenario: an upgrade at a new path removes the superseded file and reports it
    Given a completed download containing "Movie.2020.2160p.BluRay.mkv"
    And the library already holds "movie/Movie_(2020)/Movie_(2020)-1080p.mkv" with contents "old"
    And the domain maps "Movie.2020.2160p.BluRay.mkv" to library path "movie/Movie_(2020)/Movie_(2020)-2160p.mkv" superseding "movie/Movie_(2020)/Movie_(2020)-1080p.mkv"
    When the download is imported
    Then 1 file is imported
    And library path "movie/Movie_(2020)/Movie_(2020)-2160p.mkv" exists
    And library path "movie/Movie_(2020)/Movie_(2020)-1080p.mkv" does not exist
    And library path "movie/Movie_(2020)/Movie_(2020)-1080p.mkv" is listed as replaced
    And library path "movie/Movie_(2020)/Movie_(2020)-2160p.mkv" is not listed as replaced

  @C20 @passing
  Scenario: an upgrade that lists its own destination as superseded never deletes what it just placed
    Given a completed download containing "Movie.2020.mkv"
    And the library already holds "movie/Movie.mkv" with contents "old"
    And the domain maps "Movie.2020.mkv" to library path "movie/Movie.mkv" superseding "movie/Movie.mkv"
    When the download is imported
    Then 1 file is imported
    And library path "movie/Movie.mkv" has the same bytes as source "Movie.2020.mkv"
    And 1 file is reported replaced

  @C20 @passing
  Scenario: a superseded file that is already gone is not reported as replaced
    Given a completed download containing "Movie.2020.mkv"
    And the domain maps "Movie.2020.mkv" to library path "movie/Movie.mkv" superseding "movie/gone.mkv"
    When the download is imported
    Then 1 file is imported
    And 0 files are reported replaced

  @C20 @passing
  Scenario: preview shows an occupied destination as a replace and lists what would be removed
    Given a completed download containing "Movie.2020.2160p.mkv"
    And the library already holds "movie/new/Movie.mkv" with contents "occupied"
    And the library already holds "movie/old/Movie.mkv" with contents "superseded"
    And the domain maps "Movie.2020.2160p.mkv" to library path "movie/new/Movie.mkv" superseding "movie/old/Movie.mkv"
    When the import is previewed
    Then the plan would replace "movie/new/Movie.mkv"
    And the plan would remove "movie/old/Movie.mkv"
    And library path "movie/old/Movie.mkv" has contents "superseded"
    And library path "movie/new/Movie.mkv" has contents "occupied"

  @C20 @passing @SKADI-T-0418
  Scenario: a replaced file goes to the recycle bin instead of being deleted (Sonarr/Radarr Recycling Bin)
    Given the operator configures a recycle bin
    And a completed download containing "Movie.2020.2160p.mkv"
    And the library already holds "movie/Movie_(2020)/Movie_(2020)-1080p.mkv" with contents "old"
    And the domain maps "Movie.2020.2160p.mkv" to library path "movie/Movie_(2020)/Movie_(2020)-2160p.mkv" superseding "movie/Movie_(2020)/Movie_(2020)-1080p.mkv"
    When the download is imported
    Then a recycle bin holds the replaced file "movie/Movie_(2020)/Movie_(2020)-1080p.mkv"

  @C20 @passing @SKADI-T-0418
  Scenario: the folder a superseded file leaves empty is pruned (Radarr "Delete empty folders")
    Given a completed download containing "Movie.2020.2160p.mkv"
    And the library already holds "movie/Movie_(2020)/Theatrical/Movie_(2020).mkv" with contents "old"
    And the domain maps "Movie.2020.2160p.mkv" to library path "movie/Movie_(2020)/Remastered/Movie_(2020)-Remastered.mkv" superseding "movie/Movie_(2020)/Theatrical/Movie_(2020).mkv"
    When the download is imported
    Then library path "movie/Movie_(2020)/Theatrical/Movie_(2020).mkv" does not exist
    And library path "movie/Movie_(2020)/Theatrical" does not exist

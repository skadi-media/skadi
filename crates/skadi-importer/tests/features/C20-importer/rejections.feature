Feature: Sample, size-floor and free-space rejections (C20)
  A shared floor runs before the domain matcher: sample names, an optional size
  floor, and a free-space reserve. Every rejection carries a reason. Mirrors
  Sonarr/Radarr sample detection and "Minimum Free Space".

  @C20 @passing
  Scenario: a file with a sample token in its name is rejected before the domain is asked
    Given a completed download containing "Movie.2020.1080p-sample.mkv"
    And the domain would place "Movie.2020.1080p-sample.mkv" at library path "movie/Movie.mkv" if asked
    When the download is imported
    Then nothing is imported
    And source "Movie.2020.1080p-sample.mkv" is rejected with reason containing "sample"
    And library path "movie/Movie.mkv" does not exist

  @C20 @passing
  Scenario: a file inside a Sample folder is rejected
    Given a completed download containing "Sample/movie.mkv"
    And the domain would place "movie.mkv" at library path "movie/Movie.mkv" if asked
    When the download is imported
    Then nothing is imported
    And source "Sample/movie.mkv" is rejected with reason containing "sample"

  @C20 @passing
  Scenario: files below the configured size floor are rejected with the size in the reason
    Given a completed download containing "Movie.2020.mkv"
    And the completed download also contains "extras/featurette.mkv" of 10 bytes
    And the importer rejects files smaller than 100 bytes
    And the domain maps "Movie.2020.mkv" to library path "movie/Movie.mkv"
    And the domain would place "featurette.mkv" at library path "movie/featurette.mkv" if asked
    When the download is imported
    Then nothing is imported
    And source "Movie.2020.mkv" is rejected with reason containing "below size floor"
    And source "extras/featurette.mkv" is rejected with reason containing "10 < 100 bytes"

  @C20 @passing
  Scenario: the size floor is off by default so tiny audiobook chapters import
    Given a completed download containing "chapter.mp3"
    And the domain maps "chapter.mp3" to library path "audiobook/book/chapter.mp3"
    When the download is imported
    Then 1 file is imported

  @C20 @passing
  Scenario: a file the domain cannot match is rejected with a reason
    Given a completed download containing "readme.txt"
    When the download is imported
    Then source "readme.txt" is rejected with reason containing "no matching acquirable"

  @C20 @passing
  Scenario: a placement that would breach the free-space reserve is rejected with the numbers
    Given a completed download containing "Movie.2020.mkv"
    And the importer keeps an unmeetable free-space reserve
    And the domain maps "Movie.2020.mkv" to library path "movie/Movie.mkv"
    When the download is imported
    Then nothing is imported
    And a rejection reason contains "insufficient free space"
    And library path "movie/Movie.mkv" does not exist

  @C20 @passing @SKADI-T-0414
  Scenario: an insufficient-space rejection is keyed by the source file like every other rejection
    Given a completed download containing "Movie.2020.mkv"
    And the importer keeps an unmeetable free-space reserve
    And the domain maps "Movie.2020.mkv" to library path "movie/Movie.mkv"
    When the download is imported
    Then a rejection names the source file "Movie.2020.mkv"

  @C20 @passing
  Scenario: the free-space decision keeps the reserve and treats an unmeasurable filesystem as fitting
    When a 100-byte placement is checked against 150 available with a 40 reserve
    Then the space verdict is that it fits
    When a 100-byte placement is checked against 150 available with a 60 reserve
    Then the space verdict is insufficient with 90 available
    When a 100-byte placement is checked when free space cannot be measured
    Then the space verdict is that it fits

  @C20 @passing
  Scenario: every source file is accounted for exactly once across the outcome
    Given a completed download containing "Movie.2020.mkv"
    And the completed download also contains "Movie.2020-sample.mkv"
    And the completed download also contains "readme.txt"
    And the domain maps "Movie.2020.mkv" to library path "movie/Movie.mkv"
    When the download is imported
    Then 1 file is imported
    And 2 files are rejected
    And every source file appears in exactly one outcome bucket

  @C20 @passing @SKADI-T-0416
  Scenario: the free-space reserve comes from operator settings (Sonarr "Minimum Free Space")
    Given the operator configures a minimum free space of 1000000000000 bytes in settings
    And a completed download containing "Movie.2020.mkv"
    And the domain maps "Movie.2020.mkv" to library path "movie/Movie.mkv"
    When the download is imported
    Then a rejection reason contains "insufficient free space"

  @C20 @passing @SKADI-T-0416
  Scenario: the file-size floor comes from operator settings
    Given the operator configures a minimum file size of 5000000 bytes in settings
    And a completed download containing "Movie.2020.mkv"
    And the domain maps "Movie.2020.mkv" to library path "movie/Movie.mkv"
    When the download is imported
    Then nothing is imported

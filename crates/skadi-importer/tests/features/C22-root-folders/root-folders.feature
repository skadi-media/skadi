Feature: The single library root and its health (C22)
  Skadi derives one root per domain under `library.root` (SKADI-T-0302) instead of
  Sonarr/Radarr's operator-managed root list. Root health (exists / directory /
  writable) and free space are probed on demand.

  @C22 @passing
  Scenario: each domain's root is derived under the single library root
    Given the operator's single library root is the scratch library
    When the movie domain derives its root
    Then the derived root is library path "movie"
    When the television domain derives its root
    Then the derived root is library path "television"
    When the audiobook domain derives its root
    Then the derived root is library path "audiobook"

  @C22 @passing
  Scenario: a writable directory is a usable root and the probe leaves nothing behind
    Given library path "movie" is a directory
    When library path "movie" is probed as a root folder
    Then the root is usable
    And the probe left nothing behind in library path "movie"

  @C22 @passing
  Scenario: a missing root reports that it does not exist
    When library path "missing" is probed as a root folder
    Then the root problem is "path does not exist"

  @C22 @passing
  Scenario: a root that is a regular file reports that it is not a directory
    Given library path "not-a-dir" is a regular file
    When library path "not-a-dir" is probed as a root folder
    Then the root problem is "path is not a directory"

  @C22 @passing
  Scenario: a read-only root reports that it is not writable
    Given library path "ro" is a read-only directory
    When library path "ro" is probed as a root folder
    Then the root problem is "path is not writable"

  @C22 @passing
  Scenario: free space is reported for a root and for a subfolder that does not exist yet
    When free space is measured for library path ""
    Then a free-space figure is reported
    When free space is measured for library path "movie/not/yet/created"
    Then a free-space figure is reported

  # These two were @gap scenarios asserting Sonarr/Radarr multi-root parity, and
  # they would have failed forever: skadi decided against a root registry
  # (SKADI-A-0004 / SKADI-T-0302). Inverted so the decision is enforced by a test
  # rather than left as a permanent red mark nobody would ever clear
  # (SKADI-T-0425).
  @C22 @passing @SKADI-T-0425
  Scenario: there is one library root per domain and no registry to add another to
    Given the operator's single library root is the scratch library
    Then every domain root derives from the single library root
    And there is no way to register a second root alongside it

  @C22 @passing @SKADI-T-0425
  Scenario: a movie cannot be anchored to a root other than its domain default
    Given the operator's single library root is the scratch library
    Then every movie derives the same root, so no per-item choice exists

  @C22 @passing @SKADI-T-0417
  Scenario: an import into a missing root is refused instead of creating the root (Sonarr "root folder is missing")
    Given the importer's library root is "vanished-root"
    And a completed download containing "Movie.2020.mkv"
    And the domain maps "Movie.2020.mkv" to library path "vanished-root/movie/Movie.mkv"
    When the download is imported
    Then nothing is imported
    And library path "vanished-root" does not exist
    And a rejection reason contains "is unusable: path does not exist"

  @C22 @passing @SKADI-T-0417
  Scenario: a live root whose mount marker has gone is refused (dropped NAS mount)
    Given the importer's library root is "library-root"
    And the library root carries a skadi root marker
    And the operator requires a root marker
    And a completed download containing "Movie.2020.mkv"
    And the domain maps "Movie.2020.mkv" to library path "library-root/movie/Movie.mkv"
    When the download is imported
    Then 1 file is imported

  @C22 @passing @SKADI-T-0417
  Scenario: an empty writable mount point without the marker is refused, not filled
    Given the importer's library root is "library-root"
    And the library root exists but is empty
    And the operator requires a root marker
    And a completed download containing "Movie.2020.mkv"
    And the domain maps "Movie.2020.mkv" to library path "library-root/movie/Movie.mkv"
    When the download is imported
    Then nothing is imported
    And a rejection reason contains "refusing to write"

  @C22 @passing @SKADI-T-0417
  Scenario: a destination outside every configured root is refused, not scattered
    Given the importer's library root is "library-root"
    And the library root carries a skadi root marker
    And a completed download containing "Movie.2020.mkv"
    And the domain maps "Movie.2020.mkv" to library path "elsewhere/Movie.mkv"
    When the download is imported
    Then nothing is imported
    And library path "elsewhere" does not exist
    And a rejection reason contains "outside every configured library root"

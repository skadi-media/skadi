Feature: Movie matcher — which edition does a downloaded file satisfy (C25)
  Radarr's import decision "release -> movie file" at Skadi's per-edition grain.

  @C25 @passing
  Scenario: an untagged release routes to the Theatrical edition under the canonical path
    Given a matcher for "The Matrix" from 1999 with editions "Theatrical"
    When the release "The.Matrix.1999.1080p.BluRay.x264-AMIABLE.mkv" is matched from download path "/dl/The.Matrix.1999.1080p.BluRay.x264-AMIABLE.mkv"
    Then the match routes to the "Theatrical" edition
    And the destination is "/movies/the-matrix_(1999)_{tmdb-603}/theatrical/the-matrix_(1999).mkv"
    And the match carries an NFO sidecar

  @C25 @passing
  Scenario: an Extended release routes to the Extended edition in its own subfolder
    Given a matcher for "The Matrix" from 1999 with editions "Theatrical, Extended"
    When the release "The.Matrix.1999.Extended.1080p.BluRay.x264-EXT.mkv" is matched from download path "/dl/x.mkv"
    Then the match routes to the "Extended" edition
    And the destination contains "/extended/"

  @C25 @passing
  Scenario: a kind the user has not enabled on this movie yields no match and no new edition
    Given a matcher for "The Matrix" from 1999 with editions "Theatrical"
    When the release "The.Matrix.1999.Extended.1080p.BluRay.x264-EXT.mkv" is matched from download path "/dl/x.mkv"
    Then no match is emitted

  @C25 @passing
  Scenario: a user-defined regex kind matches the parsed edition token
    Given a matcher for "The Matrix" from 1999 with editions "Theatrical"
    And a custom kind "4K Scan" tagged "SW 4K Scan" matching pattern "4k\s+digital\s+film\s+scan" with its own edition
    When a release whose parsed edition is "4K Digital Film Scan" is matched from download path "/dl/x.mkv"
    Then the match routes to the "4K Scan" edition
    And the destination contains "/sw-4k-scan/"

  @C25 @passing
  Scenario: sample files are rejected by name, by folder and by size
    Given a matcher for "The Matrix" from 1999 with editions "Theatrical"
    When the release "The.Matrix.1999.1080p.BluRay.mkv" is matched from download path "/dl/The.Matrix/Sample-The.Matrix.mkv"
    Then no match is emitted
    When the release "The.Matrix.1999.1080p.BluRay.mkv" is matched from download path "/dl/The.Matrix/Sample/main.mkv"
    Then no match is emitted
    When a 1024-byte file named "The.Matrix.1999.1080p.BluRay.mkv" is matched
    Then no match is emitted

  @C25 @passing
  Scenario: an upgrade supersedes the existing edition file and overwrites on collision
    Given a matcher for "The Matrix" from 1999 with editions "Theatrical"
    And the Theatrical edition already holds "/movies/the-matrix_(1999)_{tmdb-603}/theatrical/old_720p.mkv"
    When the release "The.Matrix.1999.1080p.BluRay.x264-AMIABLE.mkv" is matched from download path "/dl/x.mkv"
    Then the match supersedes "/movies/the-matrix_(1999)_{tmdb-603}/theatrical/old_720p.mkv" and overwrites on collision

  @C25 @passing
  Scenario: filesystem-unsafe title characters never reach the destination path
    Given a matcher for "Title: with / unsafe \ chars?" from 1999 with editions "Theatrical"
    When the release "title.with.unsafe.chars.1999.1080p.BluRay.mkv" is matched from download path "/dl/x.mkv"
    Then the match routes to the "Theatrical" edition
    And the destination has no filesystem-unsafe characters

  @C25 @passing @SKADI-T-0445
  Scenario: a release for a different film is not routed to this movie's edition (Radarr "unable to match to movie")
    Given a matcher for "The Matrix" from 1999 with editions "Theatrical"
    When the release "Blade.Runner.1982.1080p.BluRay.x264-GRP.mkv" is matched from download path "/dl/Blade.Runner.1982.1080p.BluRay.x264-GRP.mkv"
    Then no match is emitted

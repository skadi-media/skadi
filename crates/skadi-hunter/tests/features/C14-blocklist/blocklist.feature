Feature: Blocklist — never grab that release again
  Auto-populated on a terminal download failure or an import failure
  (REQ-BLOCKLIST.2/.3), vetoing by stable release key in `decide`
  (REQ-BLOCKLIST.5), with optional expiry (REQ-BLOCKLIST.8) purged by the
  sweep (REQ-BLOCKLIST.9).

  Background:
    Given a registered movie domain with an in-memory status sink

  @C14 @passing @serial
  Scenario: a permanent block vetoes the release and re-blocking is idempotent
    Given "Dead.Release.2020.1080p.BluRay.x264-GRP" was blocklisted for "ed-1" permanently
    When "Dead.Release.2020.1080p.BluRay.x264-GRP" is blocklisted again for "ed-1"
    Then the blocklist vetoes "Dead.Release.2020.1080p.BluRay.x264-GRP"
    And the blocklist does not veto "Other.Release.2020.1080p.BluRay.x264-GRP"
    And the blocklist holds 1 entry
    And the blocklist for "ed-1" holds 1 entry

  @C14 @passing @serial
  Scenario: an expiring block vetoes until it lapses, then is purged
    Given "Soon.2020.1080p.BluRay.x264-GRP" was blocklisted for "ed-1" expiring in 2 hours
    And "Lapsed.2020.1080p.BluRay.x264-GRP" was blocklisted for "ed-1" expiring in -1 hours
    Then the blocklist vetoes "Soon.2020.1080p.BluRay.x264-GRP"
    And the blocklist does not veto "Lapsed.2020.1080p.BluRay.x264-GRP"
    When expired blocklist entries are purged
    Then the blocklist holds 1 entry

  @C14 @passing @serial
  Scenario: a terminal download failure blocklists the release permanently
    Given a torrent client that will report:
      | status  |
      | Failed  |
    And a registered movie domain with an in-memory status sink
    And a run "run-a" for "ed-1" that chose "Movie.2020.1080p.BluRay.x264-GRP"
    When run "run-a" snatches
    And run "run-a" monitors
    Then "Movie.2020.1080p.BluRay.x264-GRP" is blocklisted
    And the blocklist for "ed-1" holds 1 entry

  @C14 @passing @serial
  Scenario: an importer-side infrastructure error does not blocklist the release
    Given a torrent client that refuses every add
    And a registered movie domain with an in-memory status sink
    And a run "run-a" for "ed-1" that chose "Movie.2020.1080p.BluRay.x264-GRP"
    When run "run-a" snatches
    Then "Movie.2020.1080p.BluRay.x264-GRP" is not blocklisted

  @C14 @passing @SKADI-T-0436 @serial
  Scenario: an automatic block carries the policy TTL
    Given a torrent client that will report:
      | status  |
      | Failed  |
    And a registered movie domain with an in-memory status sink
    And a run "run-a" for "ed-1" that chose "Movie.2020.1080p.BluRay.x264-GRP"
    When run "run-a" snatches
    And run "run-a" monitors
    Then the blocklist entry for "Movie.2020.1080p.BluRay.x264-GRP" expires

  @C14 @gap @serial
  Scenario: a block is scoped to the item it was recorded for
    Given "Shared.2020.1080p.BluRay.x264-GRP" was blocklisted for "ed-1" permanently
    Then the blocklist still lets "Shared.2020.1080p.BluRay.x264-GRP" be grabbed for "ed-2"

  @C14 @passing @serial @SKADI-T-0529
  Scenario: a failed grab is re-searched immediately (Sonarr "Redownload Failed")
    Given a torrent client that will report:
      | status  |
      | Failed  |
    And a registered movie domain with an in-memory status sink
    And a run "run-a" for "ed-1" that chose "Movie.2020.1080p.BluRay.x264-GRP"
    When run "run-a" snatches
    And run "run-a" monitors
    Then the status of "ed-1" is Failed and immediately retryable

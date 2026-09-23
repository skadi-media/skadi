Feature: Movie status sink — persisting the acquisition lifecycle (C27)
  Radarr's per-movie downloaded/missing/queued state plus its History rows.

  Background:
    Given an empty movies library
    And a monitored movie "The Matrix" from 1999 with a Missing Theatrical edition

  @C27 @passing
  Scenario Outline: every lifecycle status round-trips through the sink
    When the hunter records <status> for the edition of "The Matrix"
    Then the persisted status of "The Matrix" reads back as <status>
    And the edition row of "The Matrix" was touched within the last minute

    Examples:
      | status      |
      | Missing     |
      | Searching   |
      | Snatched    |
      | Downloading |
      | Imported    |
      | Cutoff      |
      | Failed      |

  @C27 @passing
  Scenario: an Imported write also lands the file, quality and format score columns
    When the hunter records Imported for "The Matrix" at "Bluray-1080p" with score 17 to file "/movies/The Matrix (1999)/The Matrix (1999).mkv"
    Then the edition row of "The Matrix" carries file "/movies/The Matrix (1999)/The Matrix (1999).mkv" quality "Bluray-1080p" score 17
    And the latest history event is "imported" labelled "The Matrix"
    And the latest history event has detail "Bluray-1080p"

  @C27 @passing
  Scenario: replaying the same write after a restart is idempotent
    When the hunter records Cutoff for the edition of "The Matrix" twice
    Then the persisted status of "The Matrix" reads back as Cutoff

  @C27 @passing
  Scenario: history records grabs and failures but not progress writes
    When the hunter records Searching for the edition of "The Matrix"
    Then the acquisition history has 0 entries
    When the hunter records Snatched for the edition of "The Matrix"
    Then the latest history event is "grabbed" labelled "The Matrix"
    When the hunter records Downloading for the edition of "The Matrix"
    Then the acquisition history has 1 entry
    When the hunter records Failed for the edition of "The Matrix"
    Then the latest history event is "failed" labelled "The Matrix"
    And the latest history event has reason code "download_failed"
    And the acquisition history has 2 entries

  @C27 @passing
  Scenario: a malformed acquirable ref is a validation error on write and read
    When the hunter records Cutoff for a malformed edition ref
    Then both the write and the read are validation errors

  @C27 @passing
  Scenario: a well-formed ref for an edition that does not exist reads back nothing
    When the hunter records Cutoff for a well-formed but unknown edition ref
    Then no status is read back for it
    And the write reports not-found without touching any row

  @C27 @passing
  Scenario: the sink enforces no transition rules — a replayed Snatched overwrites Imported
    When the hunter records Imported for the edition of "The Matrix"
    And the hunter records Snatched for the edition of "The Matrix"
    Then the persisted status of "The Matrix" reads back as Snatched

  @C27 @passing
  Scenario: probed media info is persisted on the edition
    When the probe reports 1920x1080 video for "The Matrix"
    Then the edition of "The Matrix" shows resolution tier "1080p"

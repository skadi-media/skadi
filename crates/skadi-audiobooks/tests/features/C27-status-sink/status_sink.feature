Feature: Audiobook status sink — persisting the acquisition lifecycle (C27)

  Background:
    Given an empty audiobook library
    And a monitored book "The Way of Kings" by "Brandon Sanderson" with ASIN B003 that is Missing

  @C27 @passing
  Scenario Outline: every lifecycle status round-trips through the sink
    When the hunter records <status> for the file of B003
    Then the persisted status of B003 reads back as <status>

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
  Scenario: an Imported write lands the file and quality columns and a titled history row
    When the hunter records Imported for the file of B003
    Then the file row of B003 carries file "/audiobooks/x.m4b" quality "M4B-256"
    And the latest history event is "imported" labelled "The Way of Kings"
    And the latest history event has detail "M4B-256"

  @C27 @passing
  Scenario: history records grabs and failures but not progress writes
    When the hunter records Searching for the file of B003
    Then the acquisition history has 0 entries
    When the hunter records Snatched for the file of B003
    Then the latest history event is "grabbed" labelled "The Way of Kings"
    When the hunter records Failed for the file of B003
    Then the latest history event is "failed" labelled "The Way of Kings"
    And the latest history event has reason code "no_suitable_release"
    And the acquisition history has 2 entries

  @C27 @passing
  Scenario: replaying the same write after a restart is idempotent
    When the hunter records Cutoff for the file of B003 twice
    Then the persisted status of B003 reads back as Cutoff

  @C27 @passing
  Scenario: the sink enforces no transition rules — a replayed Snatched overwrites Imported
    When the hunter records Imported for the file of B003
    And the hunter records Snatched for the file of B003
    Then the persisted status of B003 reads back as Snatched

  @C27 @passing
  Scenario: malformed and unknown refs
    When the hunter records Cutoff for a malformed file ref
    Then both the write and the read are validation errors
    When the hunter records Cutoff for a well-formed but unknown file ref
    Then no status is read back for it and the write reports not-found

  @C27 @passing
  Scenario: probed media info is persisted on the file
    When the probe reports a 128 kbps aac stream for B003
    Then the file of B003 shows a 128 kbps audio stream

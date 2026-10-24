Feature: Episode status sink — persisting the acquisition lifecycle (C27)

  Background:
    Given an empty television library
    And a monitored series "Game of Thrones" from 2011
    And "Game of Thrones" has episode S1E5 aired on 2011-05-15 that is Missing

  @C27 @passing
  Scenario Outline: every lifecycle status round-trips through the sink
    When the hunter records <status> for episode S1E5 of "Game of Thrones"
    Then the persisted status of S1E5 of "Game of Thrones" reads back as <status>

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
  Scenario: an Imported write lands the file and quality columns and a labelled history row
    When the hunter records Imported for episode S1E5 of "Game of Thrones"
    Then the episode row of S1E5 of "Game of Thrones" carries file "/tv/x.mkv" quality "Bluray-1080p"
    And the latest history event is "imported" labelled "Game of Thrones - S01E05"

  @C27 @passing
  Scenario: history records grabs and failures but not progress writes
    When the hunter records Searching for episode S1E5 of "Game of Thrones"
    Then the acquisition history has 0 entries
    When the hunter records Snatched for episode S1E5 of "Game of Thrones"
    Then the latest history event is "grabbed" labelled "Game of Thrones - S01E05"
    When the hunter records Failed for episode S1E5 of "Game of Thrones"
    Then the latest history event is "failed" labelled "Game of Thrones - S01E05"
    And the latest history event has reason code "import_failed"
    And the acquisition history has 2 entries

  @C27 @passing
  Scenario: replaying the same write after a restart is idempotent
    When the hunter records Cutoff for episode S1E5 of "Game of Thrones" twice
    Then the persisted status of S1E5 of "Game of Thrones" reads back as Cutoff

  @C27 @passing
  Scenario: the sink enforces no transition rules — a replayed Snatched overwrites Imported
    When the hunter records Imported for episode S1E5 of "Game of Thrones"
    And the hunter records Snatched for episode S1E5 of "Game of Thrones"
    Then the persisted status of S1E5 of "Game of Thrones" reads back as Snatched

  @C27 @passing
  Scenario: malformed and unknown refs
    When the hunter records Cutoff for a malformed episode ref
    Then both the write and the read are validation errors
    When the hunter records Cutoff for a well-formed but unknown episode ref
    Then no status is read back for it and the write reports not-found

  @C27 @passing @SKADI-T-0451
  Scenario: probed media info is persisted for an episode like it is for movies and audiobooks
    When the probe reports 1920x1080 video for episode S1E5 of "Game of Thrones"
    Then the probed media info is persisted for episode S1E5 of "Game of Thrones"

  @C27 @passing @SKADI-T-0743
  Scenario: an import that replaced a held file is marked as an upgrade
    When the import step records Imported for episode S1E5 of "Game of Thrones" as an upgrade
    Then the latest history event is "imported" labelled "Game of Thrones - S01E05"
    And the acquisition history has 1 upgrade

  @C27 @passing @SKADI-T-0743
  Scenario: a first import is not an upgrade
    When the import step records Imported for episode S1E5 of "Game of Thrones"
    Then the acquisition history has 1 entry
    And the acquisition history has 0 upgrades

  @C27 @passing @SKADI-T-0743
  Scenario: a resumed run that writes the same import again adds no second history row
    When the import step records Imported for episode S1E5 of "Game of Thrones"
    And the import step records Imported for episode S1E5 of "Game of Thrones"
    Then the acquisition history has 1 entry

  @C27 @passing @SKADI-T-0743
  Scenario: an upgrade to the same file and quality still adds a row
    When the import step records Imported for episode S1E5 of "Game of Thrones"
    And the import step records Imported for episode S1E5 of "Game of Thrones" as an upgrade
    Then the acquisition history has 2 entries
    And the acquisition history has 1 upgrade

  @C27 @passing @SKADI-T-0743
  Scenario: a grab is never marked as an upgrade
    When the hunter records Snatched for episode S1E5 of "Game of Thrones"
    Then the acquisition history has 0 upgrades

  @C27 @passing @SKADI-T-0743
  Scenario: an upload placed by library import writes one imported row
    When library import places an upload for episode S1E5 of "Game of Thrones"
    Then the latest history event is "imported" labelled "Game of Thrones - S01E05"
    And the acquisition history has 1 entry
    And the acquisition history has 0 upgrades

  @C27 @passing @SKADI-T-0743
  Scenario: a season pack import writes one row per episode and none for the pack
    Given "Game of Thrones" has episode S1E6 aired on 2011-05-22 that is Missing
    When the import step records Imported for the season 1 pack of "Game of Thrones"
    Then the acquisition history has 0 entries
    When the import step records Imported for episode S1E5 of "Game of Thrones"
    And the import step records Imported for episode S1E6 of "Game of Thrones"
    Then the acquisition history has 2 entries
    And the acquisition history has an "imported" row labelled "Game of Thrones - S01E05"
    And the acquisition history has an "imported" row labelled "Game of Thrones - S01E06"

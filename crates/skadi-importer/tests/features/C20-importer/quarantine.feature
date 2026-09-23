Feature: Quarantine of probable-but-ambiguous media (C20)
  A domain that recognises wanted media it cannot confidently assign returns
  Quarantine; the importer hardlinks it into the domain's review area for manual
  import instead of guessing a library path (SKADI-T-0309/0311).

  @C20 @passing
  Scenario: an ambiguous file is hardlinked into the review area and reported quarantined
    Given a completed download containing "Book 3.m4b"
    And the domain quarantines "Book 3.m4b" into review path "audiobook/_review/Book 3.m4b"
    When the download is imported
    Then nothing is imported
    And nothing is rejected
    And 1 file is quarantined
    And library path "audiobook/_review/Book 3.m4b" is listed as quarantined
    And library path "audiobook/_review/Book 3.m4b" is a hardlink of source "Book 3.m4b"
    And source "Book 3.m4b" still exists

  @C20 @passing
  Scenario: a quarantine placement failure is isolated to that file
    Given a completed download containing "Book 3.m4b"
    And the completed download also contains "Book 4.m4b"
    And the library directory "audiobook/_review" is read-only
    And the domain quarantines "Book 3.m4b" into review path "audiobook/_review/Book 3.m4b"
    And the domain maps "Book 4.m4b" to library path "audiobook/book-4/book-4.m4b"
    When the download is imported
    Then 1 file is imported
    And 0 files are quarantined
    And source "Book 3.m4b" is reported failed with reason containing "quarantine placement failed"

  @C20 @passing @SKADI-T-0413
  Scenario: quarantining a second file with the same name must not silently overwrite the first
    Given a completed download containing "Book 3.m4b"
    And the library already holds "audiobook/_review/Book 3.m4b" with contents "first quarantined copy"
    And the domain quarantines "Book 3.m4b" into review path "audiobook/_review/Book 3.m4b"
    When the download is imported
    Then library path "audiobook/_review/Book 3.m4b" has contents "first quarantined copy"

  @C20 @passing
  Scenario: preview lists what would be quarantined without touching the review area
    Given a completed download containing "Book 3.m4b"
    And the domain quarantines "Book 3.m4b" into review path "audiobook/_review/Book 3.m4b"
    When the import is previewed
    Then the plan would quarantine "audiobook/_review/Book 3.m4b"
    And the library root contains no files

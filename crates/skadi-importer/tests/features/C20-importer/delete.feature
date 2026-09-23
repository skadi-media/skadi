Feature: Deleting a library item's files (C20)
  The file side of a library delete removes exactly the listed files and prunes
  the folders they leave empty, never the root and never a sibling.

  @C20 @passing
  Scenario: deleting a book prunes its now-empty folder but keeps siblings and the root
    Given the library holds "audiobook/author/series/book/part1.m4b"
    And the library holds "audiobook/author/series/book/part2.m4b"
    And the library holds "audiobook/author/series/other/keep.m4b"
    When the library item's files "audiobook/author/series/book/part1.m4b, audiobook/author/series/book/part2.m4b" are deleted
    Then 2 files were removed
    And library path "audiobook/author/series/book" does not exist
    And library path "audiobook/author/series/other/keep.m4b" exists
    And library path "audiobook/author/series" exists
    And the library root still exists

  @C20 @passing
  Scenario: deleting a file that is already gone is a safe no-op
    When the library item's files "movie/nope.mkv" are deleted
    Then 0 files were removed
    And the library root still exists

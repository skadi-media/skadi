Feature: Audiobook chapter marks (C20 media-info)
  Chapter extraction reads the QuickTime chapter text track of an MP4-family
  audiobook; files without chapters, or that are not MP4 at all, yield an empty
  list rather than an error so the player degrades to a plain seek bar.

  @C20 @passing
  Scenario: an audio file without a chapter track yields no chapters and no error
    Given the fixture "tiny.m4a"
    When chapters are extracted
    Then chapter extraction does not error
    And 0 chapters are found

  @C20 @passing
  Scenario: a garbage file yields no chapters and no error
    Given a file named "junk.m4b" containing 4096 bytes of garbage
    When chapters are extracted
    Then chapter extraction does not error
    And 0 chapters are found

  @C20 @passing
  Scenario: an empty file yields no chapters and no error
    Given an empty file named "empty.m4b"
    When chapters are extracted
    Then chapter extraction does not error
    And 0 chapters are found

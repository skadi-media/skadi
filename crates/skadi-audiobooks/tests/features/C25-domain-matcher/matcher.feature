Feature: Audiobook matcher — which book file does a downloaded file satisfy (C25)
  Readarr's import decision for a book, plus Skadi's series-pack fan-out and review quarantine.

  @C25 @passing
  Scenario: a single m4b is renamed to the title under the author folder
    Given a matcher for the book "The Way of Kings" by "Brandon Sanderson" with ASIN B003
    When the downloaded file "/dl/Brandon Sanderson - The Way of Kings.m4b" is matched
    Then the match routes to the file of B003
    And the destination is "/audiobooks/brandon-sanderson/the-way-of-kings_{asin-B003}/the-way-of-kings.m4b"

  @C25 @passing
  Scenario: a multi-file mp3 book keeps chapter names inside a series folder
    Given a matcher for the book "The Way of Kings" by "Brandon Sanderson" with ASIN B003 in series "Stormlight Archive" at position "1"
    And the completed download contains "/dl/WoK/Chapter_01.mp3; /dl/WoK/Chapter_02.mp3"
    When the downloaded file "/dl/WoK/Chapter_01.mp3" is matched
    Then the match routes to the file of B003
    And the destination is "/audiobooks/brandon-sanderson/stormlight-archive/1_-_the-way-of-kings_{asin-B003}/Chapter_01.mp3"

  @C25 @passing
  Scenario: excerpts and non-audio files are ignored
    Given a matcher for the book "The Way of Kings" by "Brandon Sanderson" with ASIN B003
    When the downloaded file "/dl/The Way of Kings - Sample.mp3" is matched
    Then the file is ignored
    When the downloaded file "/dl/Sample/ch1.mp3" is matched
    Then the file is ignored
    When the downloaded file "/dl/cover.jpg" is matched
    Then the file is ignored

  @C25 @passing
  Scenario: a series pack fans each file out to its own book
    Given a pack matcher over series "Dungeon Crawler Carl" by "Matt Dinniman" with books "DCC1|Dungeon Crawler Carl|1; DCC2|Carl's Doomsday Scenario|2; DCC3|The Dungeon Anarchist's Cookbook|3"
    And the completed download contains "/dl/DCC/Book 01 - Dungeon Crawler Carl.m4b; /dl/DCC/Book 02 - Carl's Doomsday Scenario.m4b; /dl/DCC/Book 03 - The Dungeon Anarchist's Cookbook.m4b"
    When the downloaded file "/dl/DCC/Book 02 - Carl's Doomsday Scenario.m4b" is matched
    Then the match routes to the file of DCC2
    When the downloaded file "/dl/DCC/Book 03 - The Dungeon Anarchist's Cookbook.m4b" is matched
    Then the match routes to the file of DCC3

  @C25 @passing
  Scenario: in a pack, an untracked volume is ignored and an ambiguous extra is quarantined
    Given a pack matcher over series "Dungeon Crawler Carl" by "Matt Dinniman" with books "DCC1|Dungeon Crawler Carl|1; DCC2|Carl's Doomsday Scenario|2"
    And the completed download contains "/dl/DCC/Book 01 - Dungeon Crawler Carl.m4b; /dl/DCC/Book 02 - Carl's Doomsday Scenario.m4b; /dl/DCC/Book 04 - The Gate of the Feral Gods.m4b; /dl/DCC/Bonus Interview.m4b"
    When the downloaded file "/dl/DCC/Book 04 - The Gate of the Feral Gods.m4b" is matched
    Then the file is ignored
    When the downloaded file "/dl/DCC/Bonus Interview.m4b" is matched
    Then the file is quarantined for review

  @C25 @passing
  Scenario: a single book in a known series is not mistaken for a pack
    Given a pack matcher over series "Dungeon Crawler Carl" by "Matt Dinniman" with books "DCC1|Dungeon Crawler Carl|1"
    And the completed download contains "/dl/DCC1/Chapter_01.mp3; /dl/DCC1/Chapter_02.mp3"
    When the downloaded file "/dl/DCC1/Chapter_02.mp3" is matched
    Then the match routes to the file of DCC1

  @C25 @passing @SKADI-T-0445
  Scenario: a download of a different book is not placed as this book (Readarr rejects an unmatched title)
    Given a matcher for the book "The Way of Kings" by "Brandon Sanderson" with ASIN B003
    When the downloaded file "/dl/Andy Weir - Project Hail Mary.m4b" is matched
    Then no match is emitted

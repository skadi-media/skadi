Feature: Audiobook catalog CRUD — authors, series, books, files, works and watchers (C24)
  Readarr's Author -> Book -> BookFile library plus Skadi's catalog works and watchers.

  Background:
    Given an empty audiobook library

  @C24 @passing
  Scenario: authors are found by ASIN and by id, and list their books
    Given an author "Brandon Sanderson" with ASIN AUTH1
    And a monitored book "The Way of Kings" by "Brandon Sanderson" with ASIN B003 that is Missing
    And a monitored book "Words of Radiance" by "Brandon Sanderson" with ASIN B004 that is Missing
    And the book B003 is attributed to author "Brandon Sanderson"
    And the book B004 is attributed to author "Brandon Sanderson"
    Then the author "Brandon Sanderson" is found by ASIN AUTH1 and by id
    And listing monitored authors yields 1
    And the books of author "Brandon Sanderson" are "B003, B004"

  @C24 @passing
  Scenario: listing filters on the monitored flag and hydrates files
    Given a monitored book "The Way of Kings" by "Brandon Sanderson" with ASIN B003 that is Missing
    And an unmonitored book "Quiet" by "Nobody" with ASIN B0Q1 that is Missing
    When the library is listed with monitored filter true
    Then the listing contains B003 with 1 file
    And the listing does not contain B0Q1
    When the library is listed with monitored filter any
    Then the listing contains B0Q1 with 1 file

  @C24 @passing
  Scenario: a book is found by ASIN with its series link
    Given a monitored book "The Way of Kings" by "Brandon Sanderson" with ASIN B003 that is Missing
    And the book B003 belongs to series "Stormlight Archive" at position "1"
    When the book B003 is looked up by ASIN
    Then the lookup returns "The Way of Kings" in series "Stormlight Archive" at position "1"

  @C24 @passing
  Scenario: a book has exactly one acquirable file
    Given a monitored book "The Way of Kings" by "Brandon Sanderson" with ASIN B003 that is Missing
    When a second file is added to the same book
    Then the file write is rejected

  @C24 @passing
  Scenario: deleting a book removes it and its file rows
    Given a monitored book "Gone" by "A" with ASIN B0G1 that is Missing
    When the book B0G1 is deleted
    And the book B0G1 is looked up by ASIN
    Then the lookup returns nothing
    And no file rows remain for B0G1

  @C24 @passing
  Scenario: the Acquirable contract wants Missing and Failed files but not Imported ones
    Given a monitored book "Missing One" by "A" with ASIN B0M1 that is Missing
    And a monitored book "Held One" by "A" with ASIN B0H1 imported at "M4B-256"
    And a monitored book "Failed One" by "A" with ASIN B0F1 that failed with no suitable release
    Then the file of B0M1 is wanted by the acquirable contract
    And the file of B0F1 is wanted by the acquirable contract
    And the file of B0H1 is unwanted by the acquirable contract

  @C24 @passing
  Scenario: catalog works are keyed by ASIN and listed per author
    Given a known work W1 "Blood of Elves" by author ASIN AUTH1 in series ASIN SER1
    And a known work W2 "Blood of Elves" by author ASIN AUTH1 in series ASIN SER1
    And a known work W3 "Other" by author ASIN AUTH2 in series ASIN SER2
    Then the works of author ASIN AUTH1 are "W1, W2"

  @C24 @passing
  Scenario: watchers are set, listed, queried and cleared per scope
    When a author watcher is set on AUTH1
    And a series watcher is set on SER1
    Then AUTH1 is currently watched at author scope
    And AUTH1 is not watched at series scope
    And 2 watchers are listed
    When the author watcher on AUTH1 is cleared
    Then AUTH1 is not watched at author scope
    And 1 watcher is listed

  @C24 @gap
  Scenario: the audiobook domain contributes items to the unified library like movies and television do
    Then the audiobook domain contributes items to the unified library

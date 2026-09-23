Feature: Audiobook metadata sync — add and refresh a book from the provider (C26)
  Readarr's AddBook / RefreshBook over the Audnexus-shaped record, with series linking.

  Background:
    Given an empty audiobook library
    And the book provider returns "The Way of Kings" by "Brandon Sanderson" narrated by "Michael Kramer" released 2010-08-31

  @C26 @passing
  Scenario: adding a book by ASIN seeds one Missing file and links the series
    Given the provider record places it in series "Stormlight Archive" at position "1"
    When the book with ASIN B003 is added from the provider
    Then the library holds "The Way of Kings" by "Brandon Sanderson" from 2010 with one Missing file
    And the book is linked to series "Stormlight Archive" at position "1"

  @C26 @passing
  Scenario: adding the same ASIN twice is rejected
    When the book with ASIN B003 is added from the provider
    And the book with ASIN B003 is added from the provider
    Then the add is rejected as a duplicate naming "The Way of Kings"

  @C26 @passing
  Scenario: a refresh re-pulls provider fields, re-links the series and keeps the user's library state
    When the book with ASIN B003 is added from the provider
    Given the book B003 was unmonitored by the user
    And the book provider returns "The Way of Kings (Unabridged)" by "Brandon Sanderson" narrated by "Kate Reading" released 2010-08-31
    And the provider record places it in series "The Stormlight Archive" at position "1"
    When the book B003 is refreshed from the provider
    Then the refreshed book is titled "The Way of Kings (Unabridged)" and keeps the user's monitored flag, profile and root folder
    And the book is linked to series "The Stormlight Archive" at position "1"

  @C26 @passing
  Scenario: a record without a series clears the link
    Given the provider record places it in series "Stormlight Archive" at position "1"
    When the book with ASIN B003 is added from the provider
    Given the provider record has no series
    When the book B003 is refreshed from the provider
    Then the book is linked to no series

  @C26 @passing
  Scenario: refreshing a book that does not exist yet needs provider defaults
    When a fresh book is refreshed without provider defaults
    Then the refresh is rejected as a validation error

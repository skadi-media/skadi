Feature: C31 library, detail drawer, activity and history
  The Movies page is a poster grid over /movies; the detail drawer offers
  interactive search (/releases), grab (/grab), reset and blocklist; Activity
  polls /activity, /downloads and /history.

  @C31 @passing
  Scenario: add → interactive search → grab → import shows as owned
    Given the harness daemon with the movies domain enabled
    And the movie "The Matrix" (tmdb 603) is added without an automatic search
    When the operator opens "/movies"
    And opens the first poster tile
    Then the drawer title contains "The Matrix"
    When the operator clicks "Search releases"
    Then the first release row contains "The.Matrix"
    When the operator clicks "Grab" on the first release row
    Then the drawer note contains "Grabbed"
    And within 40 seconds the Movies page reports 1 owned

  @C31 @passing
  Scenario: the activity page lists the grab and the import in history
    Given the harness daemon with the movies domain enabled
    And the movie "The Matrix" (tmdb 603) has been grabbed and imported
    When the operator opens "/activity"
    Then the history table has a row with event "grabbed"
    And the history table has a row with event "imported"

  @C31 @passing
  Scenario: the Add page searches the metadata provider and offers Add
    Given the harness daemon with the movies domain enabled
    When the operator opens "/add"
    And searches movies for "Matrix"
    Then a result row contains "The Matrix"
    And the result row offers an "Add" action

  @C31 @gap
  Scenario: the library grid pages instead of loading every item
    Given the harness daemon with the movies domain enabled
    And 120 movies are in the library
    When the operator opens "/movies"
    Then the page shows page controls
    And at most 60 poster tiles are rendered

  @C31 @gap
  Scenario: bulk edit of monitored state across selected items
    Given the harness daemon with the movies domain enabled
    And 3 movies are in the library
    When the operator opens "/movies"
    And selects every tile
    Then a bulk action "Unmonitor" is offered

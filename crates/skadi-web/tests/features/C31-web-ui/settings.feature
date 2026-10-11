Feature: C31 settings pages
  Settings → Indexers / Downloaders / Profiles / Notifiers / Custom formats are
  CRUD forms over /settings/{kind}; secrets are write-only (has_secret marker);
  the Test button drives /settings/{kind}/{id}/test.

  @C31 @passing
  Scenario: a quality profile is created from the form and listed
    Given the harness daemon with the movies domain enabled
    When the operator opens "/movies/config"
    And fills the profile form with name "HD" and cutoff "Bluray-1080p"
    And submits the form
    Then the profiles list contains "HD"

  @C31 @passing
  Scenario: an indexer's api key is never echoed back into the edit form
    Given the harness daemon with the movies domain enabled
    And a stored torznab indexer "nzbgeek" with an api key
    When the operator opens "/indexers"
    And opens the editor for "nzbgeek"
    Then the api key field is empty
    And the form shows a stored-secret indicator

  @C31 @passing
  Scenario: testing a dead indexer reports the failure inline
    Given the harness daemon with the movies domain enabled
    And a stored torznab indexer "dead" pointing at a closed port
    When the operator opens "/indexers"
    And clicks "Test" for "dead"
    Then the row for "dead" shows a failure message

  @C31 @passing @SKADI-T-0720
  Scenario: the theme choice changes the page at once and holds after a reload
    When the operator opens "/indexers"
    Then the theme toggle offers "Light", "Dark", "Hyper" and "System", and marks "System"
    And <html> has no data-theme
    When the operator picks "Light", "Dark" and "Hyper" in turn
    Then <html> has that data-theme and the toggle marks it, before and after a reload
    And the stored choice is on <html> before the app runs
    When the operator picks "System"
    Then <html> has no data-theme, nothing is stored, and the page follows the operating system

  @C31 @gap
  Scenario: settings changes show a "reloading providers" confirmation
    Given the harness daemon with the movies domain enabled
    When the operator opens "/indexers"
    And saves any indexer change
    Then the page shows a toast containing "applied"

Feature: Movies metadata sync — add and refresh from the provider (C26)
  Radarr's AddMovieService / RefreshMovieService over a provider-neutral record.

  Background:
    Given an empty movies library
    And the metadata provider returns "The Matrix" released 1999 running 136 minutes

  @C26 @passing
  Scenario: adding a movie by TMDB id seeds exactly one Missing Theatrical edition
    When "The Matrix" is added by TMDB id 603
    Then the library holds "The Matrix" from 1999 with one Missing Theatrical edition

  @C26 @passing
  Scenario: adding the same TMDB id twice is rejected without a second row
    When "The Matrix" is added by TMDB id 603
    And "The Matrix" is added by TMDB id 603
    Then the add is rejected as a duplicate naming "The Matrix"
    And only one movie row exists for "The Matrix"

  @C26 @passing
  Scenario: a refresh re-pulls provider fields but keeps the user's library state
    When "The Matrix" is added by TMDB id 603
    Given the movie "The Matrix" was unmonitored by the user with imdb "tt0133093"
    And the metadata provider returns "The Matrix Reloaded" released 2003 running 138 minutes
    And the provider record carries imdb "tt9999999"
    When the movie "The Matrix" is refreshed from the provider
    Then the refreshed movie is titled "The Matrix Reloaded" from 2003
    And the refreshed movie keeps the user's monitored flag, profile, root folder and imdb "tt0133093"
    And the refreshed movie is stamped with a metadata refresh time

  @C26 @passing
  Scenario: a missing release date leaves the year unset rather than failing
    Given the provider record has no release date
    When "The Matrix" is added by TMDB id 603
    Then the refreshed movie has no year

  @C26 @passing
  Scenario: poster and backdrop references are captured from the record
    Given the provider record carries a poster "https://img/poster.jpg" and backdrop "https://img/backdrop.jpg"
    When "The Matrix" is added by TMDB id 603
    Then the refreshed movie carries poster "https://img/poster.jpg" and backdrop "https://img/backdrop.jpg"

  @C26 @passing
  Scenario: refreshing a movie that does not exist yet needs provider defaults
    When a fresh movie is refreshed without provider defaults
    Then the refresh is rejected as a validation error
    When a fresh movie is refreshed with provider defaults
    Then the refreshed movie is titled "The Matrix" from 1999

  @C26 @passing @SKADI-T-0450
  Scenario: the operator can trigger a metadata refresh for one movie over the API
    Then the movies API exposes a per-movie metadata refresh route

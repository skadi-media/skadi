Feature: Movies catalog CRUD and the LibraryItem / Acquirable contract (C24)
  The system of record Radarr calls the Movie library, split into Movie -> MovieEdition.

  Background:
    Given an empty movies library

  @C24 @passing
  Scenario: listing filters on the monitored flag and hydrates editions
    Given a monitored movie "The Matrix" from 1999 with a Missing Theatrical edition
    And an unmonitored movie "Off the Books" from 2010 with a Missing Theatrical edition
    When the library is listed with monitored filter true
    Then the listing contains "The Matrix"
    And the listing does not contain "Off the Books"
    And every listed movie carries its editions
    When the library is listed with monitored filter any
    Then the listing contains "Off the Books"

  @C24 @passing
  Scenario: a movie is found by its TMDB id with its editions attached
    Given a monitored movie "The Matrix" from 1999 with a Missing Theatrical edition
    When an Extended edition is added to "The Matrix"
    And the movie "The Matrix" is looked up by its TMDB id
    Then the lookup returns "The Matrix" with 2 editions

  @C24 @passing
  Scenario: a movie without a TMDB id cannot be saved
    When a movie without a TMDB id is saved
    Then the save is rejected as a validation error

  @C24 @passing
  Scenario: an edition is unique per movie and kind
    Given a monitored movie "The Matrix" from 1999 with a Missing Theatrical edition
    When a second Theatrical edition is added to "The Matrix"
    Then the edition write is rejected

  @C24 @passing
  Scenario: deleting a movie removes it from the catalog
    Given a monitored movie "Gone" from 2000 with a Missing Theatrical edition
    When the movie "Gone" is deleted
    And the movie "Gone" is loaded by id
    Then the lookup returns nothing

  @C24 @passing
  Scenario: deleting a movie also removes its edition rows (FK cascade holds on SQLite)
    Given a monitored movie "Gone" from 2000 with a Missing Theatrical edition
    When the movie "Gone" is deleted
    Then no edition rows remain for "Gone"

  @C24 @passing
  Scenario: the Acquirable contract wants Missing and Failed editions but not Imported ones
    Given a monitored movie "Missing One" from 2001 with a Missing Theatrical edition
    And a monitored movie "Held One" from 2002 with a Theatrical edition imported at "Bluray-1080p"
    And a monitored movie "Failed One" from 2003 whose Theatrical edition failed with no suitable release
    Then the Theatrical edition of "Missing One" is wanted by the acquirable contract
    And the Theatrical edition of "Failed One" is wanted by the acquirable contract
    And the Theatrical edition of "Held One" is not wanted by the acquirable contract

  @C24 @passing
  Scenario: the movies domain contributes items to the unified library
    Given a monitored movie "The Matrix" from 1999 with a Theatrical edition imported at "Bluray-1080p"
    And an unmonitored movie "Off the Books" from 2010 with a Missing Theatrical edition
    When the unified library lists movies with monitored filter true
    Then the library items include "The Matrix" with 1 edition row keyed by the Theatrical kind id
    And the library item "The Matrix" reports status "imported" and quality "Bluray-1080p"
    And the library items do not include "Off the Books"

  @C24 @passing @SKADI-T-0454
  Scenario: unified-library edition rows carry human-readable kind and quality names
    Given a monitored movie "The Matrix" from 1999 with a Theatrical edition imported at "Bluray-1080p"
    When the unified library lists movies with monitored filter true
    Then the library item "The Matrix" names its edition kind "Theatrical" and quality "Bluray-1080p"

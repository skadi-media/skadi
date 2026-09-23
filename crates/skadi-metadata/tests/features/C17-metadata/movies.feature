Feature: Movie metadata providers — TMDB and the Servarr metadata API
  crates/skadi-metadata/src/tmdb.rs and servarr.rs implement MetadataProvider
  (search / lookup / refresh) for movies. Radarr equivalent: the Servarr metadata
  proxy (api.radarr.video) with TMDB behind it, refreshed on a schedule and on
  demand, with "removed from TMDB" handling. The upstream is an in-process mock.

  @C17 @passing
  Scenario: a TMDB search sends the title and year and maps results with poster, overview and popularity score
    Given a TMDB provider with API key "k"
    And the upstream answers "/search/movie?query=The Matrix&year=1999&api_key=k" with JSON:
      """
      { "results": [
        { "id": 603, "title": "The Matrix", "release_date": "1999-03-31", "popularity": 80.5, "overview": "A hacker learns the truth.", "poster_path": "/matrix.jpg" },
        { "id": 604, "title": "The Matrix Reloaded", "release_date": "2003-05-15", "popularity": 60.1, "overview": "", "poster_path": null }
      ] }
      """
    When the user searches for "The Matrix" (1999) as a "movie"
    Then 2 matches are returned
    And match 1 is "The Matrix" (1999) with tmdb id 603
    And match 1 has poster "https://image.tmdb.org/t/p/w200/matrix.jpg" and an overview
    And match 2 has no poster
    And match 1 scores higher than match 2
    And the upstream received a request with "year=1999"

  @C17 @passing
  Scenario: a TMDB lookup by TMDB id maps the movie with its IMDb id, dates, runtime, US certification and sized image URLs
    Given a TMDB provider with API key "k"
    And the upstream answers "/movie/603?append_to_response=external_ids,release_dates" with JSON:
      """
      { "id": 603, "title": "The Matrix", "original_title": "The Matrix", "overview": "x", "runtime": 136,
        "release_date": "1999-03-31", "poster_path": "/p.jpg", "backdrop_path": "/b.jpg",
        "external_ids": { "imdb_id": "tt0133093" },
        "release_dates": { "results": [
          { "iso_3166_1": "GB", "release_dates": [ { "certification": "15", "type": 3 } ] },
          { "iso_3166_1": "US", "release_dates": [ { "certification": "", "type": 1 }, { "certification": "R", "type": 3 } ] } ] } }
      """
    When the movie 603 is looked up by TMDB id
    Then the record is "The Matrix" released 1999-03-31 running 136 minutes
    And the record has tmdb id 603 and imdb id "tt0133093"
    And the record has content rating "R"
    And the record has a poster "https://image.tmdb.org/t/p/w500/p.jpg" and a backdrop "https://image.tmdb.org/t/p/w780/b.jpg"
    And the provider is named "tmdb" and supports "movie" but not "audiobook"

  @C17 @passing
  Scenario: TMDB cannot look a movie up by IMDb id (a validation error, not a request)
    Given a TMDB provider with API key "k"
    When the movie "tt0133093" is looked up by IMDb id
    Then the lookup fails with an error containing "TMDB id"
    And the upstream received 0 requests to "/movie/tt0133093"

  @C17 @passing
  Scenario: a refresh re-fetches the record (no cache in front of the provider)
    Given a TMDB provider with API key "k"
    And the upstream answers "/movie/603" with JSON:
      """
      { "id": 603, "title": "The Matrix" }
      """
    When the movie 603 is refreshed
    Then the lookup succeeds
    And the upstream received 1 request to "/movie/603"

  @C17 @passing
  Scenario: an upstream outage (5xx) is retried with backoff and then surfaces as a network error
    Given a TMDB provider with API key "k"
    And the upstream answers "/movie/603" with HTTP 503
    When the movie 603 is looked up by TMDB id
    Then the lookup fails with an error containing "HTTP 503"
    And the upstream received 4 requests to "/movie/603"

  @C17 @passing
  Scenario: an invalid API key (401) is not retried and fails the lookup
    Given a TMDB provider with API key "bad"
    And the upstream answers "/movie/603" with HTTP 401
    When the movie 603 is looked up by TMDB id
    Then the lookup fails with an error containing "HTTP 401"
    And the upstream received 1 request to "/movie/603"

  @C17 @passing
  Scenario: a malformed body is a decoding error naming the provider
    Given a TMDB provider with API key "k"
    And the upstream answers "/movie/603" with HTTP 200 and body "<html>not json</html>"
    When the movie 603 is looked up by TMDB id
    Then the lookup fails with an error containing "decoding TMDB movie"

  @C17 @passing @SKADI-T-0502
  Scenario: a movie TMDB no longer knows (404) is reported as not found, not as a network failure
    Given a TMDB provider with API key "k"
    And the upstream answers "/movie/999999" with HTTP 404
    When the movie 999999 is looked up by TMDB id
    Then the lookup reports the item as not found rather than a network failure

  @C17 @passing @SKADI-T-0510
  Scenario: a 429 with Retry-After is honoured before the retry (TMDB's rate limit)
    Given a TMDB provider with API key "k"
    And the upstream answers "/movie/603" with HTTP 429 and Retry-After 3 once, then with JSON:
      """
      { "id": 603, "title": "The Matrix" }
      """
    When the movie 603 is looked up by TMDB id
    Then the lookup succeeds
    And the lookup waited at least 3 seconds

  @C17 @passing @SKADI-T-0510
  Scenario: repeated lookups of the same id within a short window are served from a cache
    Given a TMDB provider with API key "k"
    And the upstream answers "/movie/603" with JSON:
      """
      { "id": 603, "title": "The Matrix" }
      """
    When the movie 603 is looked up by TMDB id
    And the movie 603 is looked up by TMDB id
    And the movie 603 is looked up by TMDB id
    Then the upstream received 1 request to "/movie/603"

  @C17 @passing
  Scenario: the Servarr metadata API looks movies up by TMDB or IMDb id and maps poster/fanart and the cinema date
    Given a Servarr metadata provider
    And the upstream answers "/movie/603" with JSON:
      """
      { "TmdbId": 603, "ImdbId": "tt0133093", "Title": "The Matrix", "Overview": "x", "OriginalTitle": "The Matrix",
        "Runtime": 136, "Popularity": 203.5, "Year": 1999,
        "Premier": "1999-03-24T00:00:00Z", "InCinema": "1999-03-31T00:00:00Z", "PhysicalRelease": "1999-11-25T00:00:00Z",
        "Images": [ { "CoverType": "Poster", "Url": "https://img/poster.jpg" }, { "CoverType": "Fanart", "Url": "https://img/fanart.jpg" }, { "CoverType": "Banner", "Url": "https://img/banner.jpg" } ] }
      """
    And the upstream answers "/movie/imdb/tt0133093" with JSON:
      """
      { "TmdbId": 603, "ImdbId": "tt0133093", "Title": "The Matrix", "Runtime": 136, "Year": 1999, "Premier": "1999-03-24T00:00:00Z" }
      """
    When the movie 603 is looked up by TMDB id
    Then the record is "The Matrix" released 1999-03-31 running 136 minutes
    And the record has tmdb id 603 and imdb id "tt0133093"
    And the record has a poster "https://img/poster.jpg" and a backdrop "https://img/fanart.jpg"
    When the movie "tt0133093" is looked up by IMDb id
    Then the record is "The Matrix" released 1999-03-24 running 136 minutes
    And the provider is named "servarr" and supports "movie" but not "series"

  @C17 @passing
  Scenario: the Servarr search sends q and year and maps matches
    Given a Servarr metadata provider
    And the upstream answers "/search?q=Heat&year=1995" with JSON:
      """
      [ { "TmdbId": 949, "ImdbId": "tt0113277", "Title": "Heat", "Year": 1995, "Popularity": 50.0,
          "Images": [ { "CoverType": "Poster", "Url": "https://img/heat.jpg" } ], "Overview": "LA crime" } ]
      """
    When the user searches for "Heat" (1995) as a "movie"
    Then 1 match is returned
    And match 1 is "Heat" (1995) with tmdb id 949
    And match 1 has poster "https://img/heat.jpg" and an overview

  @C17 @passing
  Scenario: the Servarr provider cannot look up by TVDB id
    Given a Servarr metadata provider
    When the item 121361 is looked up by TVDB id
    Then the lookup fails with an error containing "tmdb/imdb only"

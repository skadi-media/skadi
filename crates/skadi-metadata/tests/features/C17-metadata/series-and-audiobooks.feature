Feature: Series metadata (Skyhook, TMDB TV) and audiobook metadata (Audnexus, Audible catalog)
  skyhook.rs / tmdb.rs implement SeriesMetadataProvider (series + seasons +
  episodes, TVDB-keyed like Sonarr); audnexus.rs and audible_catalog.rs are the
  Readarr-style book/author providers for the audiobooks domain.

  @C17 @passing
  Scenario: Skyhook maps a show with its seasons, episode counts, first-air dates and ids
    Given a Skyhook provider
    And the upstream answers "/shows/en/121361" with JSON:
      """
      { "tvdbId": 121361, "tmdbId": 1399, "imdbId": "tt0944947", "title": "Game of Thrones", "firstAired": "2011-04-17",
        "status": "Ended", "network": "HBO", "runtime": 60, "genres": ["Drama", "Fantasy"], "aniListIds": [], "malIds": [],
        "images": [ {"coverType": "Poster", "url": "https://x/poster.jpg"}, {"coverType": "Banner", "url": "https://x/banner.jpg"} ],
        "seasons": [ {"seasonNumber": 0}, {"seasonNumber": 1, "name": "Winter is Coming"} ],
        "episodes": [
          {"seasonNumber": 1, "episodeNumber": 1, "title": "Winter Is Coming", "airDate": "2011-04-17"},
          {"seasonNumber": 1, "episodeNumber": 2, "title": "The Kingsroad", "airDate": "2011-04-24"},
          {"seasonNumber": 0, "episodeNumber": 1, "title": "Inside GoT", "airDate": "2010-12-05"} ] }
      """
    When the series 121361 is looked up with its seasons and episodes
    Then the series is "Game of Thrones" on "HBO" with status "Ended"
    And the series has tvdb id 121361, tmdb id 1399 and imdb id "tt0944947"
    And the series has 2 seasons and 3 episodes
    And season 1 is "Winter is Coming" with 2 episodes first airing 2011-04-17
    And the series is not flagged as anime

  @C17 @passing
  Scenario: an AniList / MAL mapping flags a show as anime and keeps absolute episode numbers
    Given a Skyhook provider
    And the upstream answers "/shows/en/278157" with JSON:
      """
      { "tvdbId": 278157, "title": "Frieren", "genres": ["Animation"], "originalCountry": "jp",
        "aniListIds": [154587], "malIds": [52991], "seasons": [{"seasonNumber": 1}],
        "episodes": [ {"seasonNumber": 1, "episodeNumber": 1, "absoluteEpisodeNumber": 1, "title": "The Journey's End"} ] }
      """
    When the series 278157 is looked up with its seasons and episodes
    Then the series is flagged as anime
    And episode S1E1 is "The Journey's End" with absolute number 1

  @C17 @passing
  Scenario: a Skyhook search maps TVDB-keyed matches with the first-air year
    Given a Skyhook provider
    And the upstream answers "/search/en?term=severance" with JSON:
      """
      [ {"tvdbId": 371980, "title": "Severance", "firstAired": "2022-02-18", "status": "Continuing", "images": [{"coverType": "Poster", "url": "https://x/sev.jpg"}], "overview": "Lumon"} ]
      """
    When the user searches for "severance" as a "series"
    Then 1 match is returned
    And match 1 is "Severance" (2022) with tvdb id 371980
    And match 1 has poster "https://x/sev.jpg" and an overview
    And the provider is named "skyhook" and supports "series" but not "movie"

  @C17 @passing
  Scenario: a Skyhook outage is retried then surfaces as a network error
    Given a Skyhook provider
    And the upstream answers "/shows/en/1" with HTTP 502
    When the series 1 is looked up with its seasons and episodes
    Then the lookup fails with an error containing "HTTP 502"

  @C17 @passing
  Scenario: TMDB TV fetches the show and then every season for its episodes
    Given a TMDB provider with API key "k"
    And the upstream answers "/tv/1399" with JSON:
      """
      { "id": 1399, "name": "Game of Thrones", "first_air_date": "2011-04-17", "status": "Ended",
        "networks": [{"name": "HBO"}], "genres": [{"name": "Drama"}], "origin_country": ["US"],
        "external_ids": {"tvdb_id": 121361, "imdb_id": "tt0944947"},
        "seasons": [ {"season_number": 1, "name": "Season 1", "episode_count": 2, "air_date": "2011-04-17"} ] }
      """
    And the upstream answers "/tv/1399/season/1" with JSON:
      """
      { "season_number": 1, "episodes": [ {"episode_number": 1, "name": "Winter Is Coming", "air_date": "2011-04-17"}, {"episode_number": 2, "name": "The Kingsroad", "air_date": "2011-04-24"} ] }
      """
    When the series 1399 is looked up with its seasons and episodes
    Then the series is "Game of Thrones" on "HBO" with status "Ended"
    And the series has 1 seasons and 2 episodes
    And the upstream received 1 request to "/tv/1399/season/1"

  @C17 @passing
  Scenario: Audnexus maps a book with authors, narrators, series position and abridgement, keyed by ASIN
    Given an Audnexus provider
    And the upstream answers "/books/B08G9PRS1K?region=us" with JSON:
      """
      { "asin": "B08G9PRS1K", "title": "Project Hail Mary", "subtitle": "A Novel",
        "authors": [{"name": "Andy Weir", "asin": "B002XLDZ6E"}], "narrators": [{"name": "Ray Porter"}],
        "seriesPrimary": {"name": "Standalones", "position": "1"}, "runtimeLengthMin": 970,
        "image": "https://m.media-amazon.com/x.jpg", "releaseDate": "2021-05-04T00:00:00.000Z",
        "formatType": "unabridged", "summary": "Ryland Grace wakes up." }
      """
    When the item "B08G9PRS1K" is looked up by ASIN
    Then the record is "Project Hail Mary" released 2021-05-04 running 970 minutes
    And the record has subtitle "A Novel"
    And the record has authors "Andy Weir" and narrators "Ray Porter"
    And the record is in series "Standalones" at position "1" and is unabridged
    And the provider is named "audnexus" and supports "audiobook" but not "movie"

  @C17 @passing
  Scenario: Audnexus only looks up by ASIN and has no title search of its own
    Given an Audnexus provider
    When the movie 603 is looked up by TMDB id
    Then the lookup fails with an error containing "ASIN"
    When the user searches for "Project Hail Mary" as a "audiobook"
    Then 0 matches are returned

  @C17 @passing
  Scenario: Audnexus author lookup and name search (de-duplicated by ASIN)
    Given an Audnexus provider
    And the upstream answers "/authors/B002XLDZ6E" with JSON:
      """
      { "asin": "B002XLDZ6E", "name": "Andy Weir", "description": "Author of The Martian.", "image": "https://x/a.jpg" }
      """
    And the upstream answers "/authors?name=Andy Weir" with JSON:
      """
      [ {"asin": "B002XLDZ6E", "name": "Andy Weir"}, {"asin": "B002XLDZ6E", "name": "Andy Weir"}, {"asin": "", "name": "ghost"}, {"asin": "B0ABC", "name": "Andy Weird"} ]
      """
    When the author "B002XLDZ6E" is looked up
    Then the author is "Andy Weir" with a description
    When authors named "Andy Weir" are searched
    Then the author matches are "B002XLDZ6E,B0ABC"

  @C17 @passing
  Scenario: the Audible catalog lists an author's products newest first, skipping rows without an ASIN or title
    Given an Audible catalog provider
    And the upstream answers "/1.0/catalog/products?author=Andy Weir" with JSON:
      """
      { "products": [
        { "asin": "B08G9PRS1K", "title": "Project Hail Mary", "release_date": "2021-05-04", "authors": [{"name": "Andy Weir", "asin": "B002XLDZ6E"}],
          "series": [{"asin": "S1", "title": "Standalones", "sequence": "1"}], "product_images": {"500": "https://x/500.jpg"} },
        { "asin": "", "title": "no asin" },
        { "asin": "B0NOTITLE", "title": "" },
        { "asin": "B002VOTBCU", "title": "The Martian", "release_date": "2014-03-22", "authors": [{"name": "Andy Weir"}] } ] }
      """
    When the catalog lists products by "Andy Weir"
    Then the catalog items are "B08G9PRS1K,B002VOTBCU"
    And catalog item 1 is "Project Hail Mary" by "Andy Weir" in series "Standalones" #1

  @C17 @passing
  Scenario: the Audible catalog free-text search and per-product language
    Given an Audible catalog provider
    And the upstream answers "/1.0/catalog/products?keywords=hail mary" with JSON:
      """
      { "products": [ { "asin": "B08G9PRS1K", "title": "Project Hail Mary" } ] }
      """
    And the upstream answers "/1.0/catalog/products/B08G9PRS1K" with JSON:
      """
      { "product": { "asin": "B08G9PRS1K", "language": "English" } }
      """
    And the upstream answers "/1.0/catalog/products/B0NOLANG" with JSON:
      """
      { "product": { "asin": "B0NOLANG" } }
      """
    When the catalog is searched for "hail mary"
    Then the catalog items are "B08G9PRS1K"
    When the language of product "B08G9PRS1K" is fetched
    Then the product language is "english"
    When the language of product "B0NOLANG" is fetched
    Then the product language is ""

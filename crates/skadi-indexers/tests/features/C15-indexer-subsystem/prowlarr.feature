Feature: Prowlarr aggregate indexer — one endpoint fanning out over every configured tracker
  crates/skadi-indexers/src/prowlarr.rs talks to Prowlarr's JSON /api/v1/search.
  Sonarr/Radarr equivalent: Prowlarr-synced indexers (but skadi registers one
  aggregate rather than one Torznab feed per tracker).

  Background:
    Given a Prowlarr aggregate indexer configured with categories "3030"

  @C15 @passing
  Scenario: a search hits the aggregate endpoint with the API key header, the query and the categories
    Given the indexer answers "/api/v1/search" with the fixture "prowlarr_search"
    And a "audiobook" query for "Dungeon Crawler Carl"
    When the hunter searches the indexer
    Then 2 releases are returned
    And the indexer received 1 request with "query=Dungeon Crawler Carl"
    And the indexer received 1 request with "categories=3030"
    And the indexer received 1 request with "type=search"

  @C15 @passing
  Scenario: a magnetUrl is preferred and a bare downloadUrl becomes a TorrentUrl the worker resolves via redirects
    Given the indexer answers "/api/v1/search" with the fixture "prowlarr_search"
    And a "audiobook" query for "Dungeon Crawler Carl"
    When the hunter searches the indexer
    Then release 1's fetch is a torrent URL containing "/15/download?link=abc"
    And release 1 has 12 seeders
    And release 1 has size 1200000000
    And release 1 carries categories "3030"
    And release 1 was published at "2026-05-12T00:00:00+00:00"
    And release 2's fetch is a magnet containing "btih:deadbeef"

  @C15 @passing
  Scenario: rows without a title or without any fetch link are dropped
    Given the indexer answers "/api/v1/search" with the fixture "prowlarr_no_title"
    And a "audiobook" query for "x"
    When the hunter searches the indexer
    Then 0 releases are returned

  @C15 @passing
  Scenario: every title alias is searched and the same release is not double-counted across aliases
    Given the indexer answers "/api/v1/search" with the fixture "prowlarr_search"
    And a "audiobook" query for "Dungeon Crawler Carl"
    And the query also has the alias "DCC"
    When the hunter searches the indexer
    Then 2 releases are returned
    And the indexer received 1 request with "query=DCC"
    And the indexer received 2 requests to "/api/v1/search"

  @C15 @passing
  Scenario: a query scoped to categories overrides the configured ones
    Given the indexer answers "/api/v1/search" with the fixture "prowlarr_empty"
    And a "audiobook" query for "x"
    And the query is scoped to categories "3000"
    When the hunter searches the indexer
    Then 0 releases are returned
    And the indexer received 1 request with "categories=3000"
    And the indexer received 0 requests with "categories=3030"

  @C15 @passing
  Scenario: the RSS pass is an empty-query aggregate search (recorded Prowlarr 2.4 fixture, newest first)
    Given a Prowlarr aggregate indexer configured with categories "2000"
    And the indexer answers "/api/v1/search" with the fixture "prowlarr_rss"
    When the hunter pulls the RSS feed
    Then 3 releases are returned
    And release 1 has 87 seeders
    And release 1's fetch is a torrent URL containing "/download"
    And the indexer received 1 request with "query="

  @C15 @passing
  Scenario: the health check probes the indexer list, never a full search; a bad key fails it
    Given the indexer answers "/api/v1/indexer" with HTTP 401
    When the indexer health check runs
    Then the health check fails with an error containing "HTTP 401"
    And the indexer received 0 requests to "/api/v1/search"

  @C15 @passing
  Scenario: capabilities are aggregated from the configured trackers' category trees
    Given the indexer answers "/api/v1/indexer" with the fixture "prowlarr_indexers"
    When the indexer capabilities are negotiated
    Then the capabilities do support search
    And the capabilities do support RSS

  @C15 @passing
  Scenario: a capabilities failure degrades to the configured categories rather than erroring
    Given the indexer answers "/api/v1/indexer" with HTTP 500
    When the indexer capabilities are negotiated
    Then the capabilities list categories "3030"

  @C15 @passing
  Scenario: a malformed JSON body is an error, not an empty result
    Given the indexer answers "/api/v1/search" with the fixture "not_xml"
    And a "audiobook" query for "x"
    When the hunter searches the indexer
    Then the search fails with an error containing "parsing Prowlarr search JSON"

  @C15 @passing
  Scenario: an aggregate outage (HTTP 503 after retries) is an error
    Given the indexer answers "/api/v1/search" with HTTP 503
    And a "audiobook" query for "x"
    When the hunter searches the indexer
    Then the search fails with an error containing "HTTP 503"

  @C15 @passing @SKADI-T-0503
  Scenario: a query carrying an IMDb id is sent as an id search (Prowlarr accepts imdbId/tmdbId/tvdbId on /api/v1/search)
    Given a Prowlarr aggregate indexer configured with categories "2000"
    And the indexer answers "/api/v1/search" with the fixture "prowlarr_empty"
    And a "movie" query for "Blade Runner" (1982)
    And the query carries imdb id "tt0083658"
    When the hunter searches the indexer
    Then the indexer saw a request whose "imdbId" parameter contains "0083658"

  @C15 @passing @SKADI-T-0503
  Scenario: a series episode query forwards season and episode (type=tvsearch&season=&ep=) instead of a bare title search
    Given a Prowlarr aggregate indexer configured with categories "5000"
    And the indexer answers "/api/v1/search" with the fixture "prowlarr_empty"
    And a "series" query for "Lioness"
    And the query asks for season 3 episode 2
    When the hunter searches the indexer
    Then the indexer saw a request whose "season" parameter contains "3"
    And the indexer saw a request whose "ep" parameter contains "2"

  @C15 @passing @SKADI-T-0499
  Scenario: a Prowlarr aggregate configured with TV categories serves the series domain
    Given a Prowlarr aggregate indexer configured with categories "5000"
    Then the indexer serves the "series" domain

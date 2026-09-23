Feature: Torznab indexer — capability negotiation, tiered search, RSS and the Release contract
  The Torznab client (crates/skadi-indexers/src/torznab.rs) is the direct-to-indexer
  protocol path (Jackett / Prowlarr per-indexer feeds / native Torznab trackers).
  Sonarr/Radarr equivalent: the Torznab indexer implementation + its caps cache.
  Every scenario talks to an in-process mock server, never a real indexer.

  Background:
    Given a Torznab indexer configured with categories "2000"

  @C15 @passing
  Scenario: capability negotiation reads supported id params and categories from t=caps
    Given the indexer answers "t=caps" with the fixture "caps"
    When the indexer capabilities are negotiated
    Then the capabilities advertise id params "imdb,tmdb"
    And the capabilities list categories "2000,2040"
    And the capabilities do support search
    And the capabilities do support RSS

  @C15 @passing
  Scenario: a search item maps onto the Release contract (title, magnet, size, seeders, categories, date)
    Given the indexer answers "t=caps" with the fixture "caps"
    And the indexer answers "t=movie" with the fixture "search_magnet"
    And a "movie" query for "Blade Runner" (1982)
    And the query carries imdb id "tt0083658"
    When the hunter searches the indexer
    Then 1 release is returned
    And release 1 has title "Blade.Runner.1982.2160p.UHD.BluRay.x265-GROUP"
    And release 1's fetch is a magnet containing "btih:deadbeef"
    And release 1 has size 15000000000
    And release 1 has 42 seeders
    And release 1 carries categories "2000,2040"
    And release 1 was published at "2023-01-10T12:00:00+00:00"
    And release 1 parsed as year 1982 and resolution "2160p"

  @C15 @passing
  Scenario: the id tier strips the "tt" prefix and, once it hits, the title tier is never requested
    Given the indexer answers "t=caps" with the fixture "caps"
    And the indexer answers "t=movie" with the fixture "search_magnet"
    And the indexer answers "t=search" with the fixture "search_two_items"
    And a "movie" query for "Blade Runner" (1982)
    And the query carries imdb id "tt0083658"
    When the hunter searches the indexer
    Then 1 release is returned
    And the indexer received 1 request with "imdbid=0083658"
    And the indexer received 0 requests with "t=search"

  @C15 @passing
  Scenario: an id tier that misses falls back to a title search carrying the year (Radarr's text-search form)
    Given the indexer answers "t=caps" with the fixture "caps"
    And the indexer answers "t=movie" with the fixture "empty_rss"
    And the indexer answers "t=search" with the fixture "search_magnet"
    And a "movie" query for "Blade Runner" (1982)
    And the query carries imdb id "tt0083658"
    When the hunter searches the indexer
    Then 1 release is returned
    And the indexer received 1 request with "q=Blade Runner 1982"
    And the indexer received 2 requests with "cat=2000"

  @C15 @passing
  Scenario: an indexer without id-search support goes straight to the title tier
    Given the indexer answers "t=caps" with the fixture "caps_tv"
    And the indexer answers "t=search" with the fixture "search_magnet"
    And a "movie" query for "Blade Runner" (1982)
    And the query carries imdb id "tt0083658"
    When the hunter searches the indexer
    Then 1 release is returned
    And the indexer received 0 requests with "t=movie"
    And the indexer received 1 request with "t=search"

  @C15 @passing
  Scenario: a .torrent link is a TorrentUrl fetch and the enclosure does not override it
    Given the indexer answers "t=caps" with the fixture "caps"
    And the indexer answers "t=search" with the fixture "search_torrent_link"
    And a "movie" query for "Heat"
    When the hunter searches the indexer
    Then 1 release is returned
    And release 1's fetch is a torrent URL containing "/1/download?apikey=k"
    And release 1 has 7 seeders
    And release 1 has size 9000000000

  @C15 @passing
  Scenario: size falls back to the torznab:attr when the <size> element is absent
    Given the indexer answers "t=caps" with the fixture "caps"
    And the indexer answers "t=search" with the fixture "search_attr_size"
    And a "movie" query for "Heat"
    When the hunter searches the indexer
    Then 1 release is returned
    And release 1 has size 4200000000
    And release 1 has unknown seeders

  @C15 @passing @SKADI-T-0507
  Scenario: size falls back to the enclosure length (Newznab's original convention) when no size attr is given
    Given the indexer answers "t=caps" with the fixture "caps"
    And the indexer answers "t=search" with the fixture "search_enclosure_length"
    And a "movie" query for "Heat"
    When the hunter searches the indexer
    Then 1 release is returned
    And release 1 has size 4200000000

  @C15 @passing @SKADI-T-0507
  Scenario: an item carrying only an infohash attribute still yields a magnet fetch (Sonarr builds the magnet)
    Given the indexer answers "t=caps" with the fixture "caps"
    And the indexer answers "t=search" with the fixture "search_infohash_only"
    And a "movie" query for "Heat"
    When the hunter searches the indexer
    Then 1 release is returned
    And release 1's fetch is a magnet containing "btih:cafebabe"

  @C15 @passing
  Scenario: items without a title or without any fetch are dropped, not returned half-built
    Given the indexer answers "t=caps" with the fixture "caps"
    And the indexer answers "t=search" with the fixture "search_no_link"
    And a "movie" query for "Heat"
    When the hunter searches the indexer
    Then 0 releases are returned

  @C15 @passing
  Scenario: unparseable size / seeders / date degrade to defaults instead of failing the whole feed
    Given the indexer answers "t=caps" with the fixture "caps"
    And the indexer answers "t=search" with the fixture "search_bad_date"
    And a "movie" query for "Heat"
    When the hunter searches the indexer
    Then 1 release is returned
    And release 1 has size 0
    And release 1 has unknown seeders
    And release 1 was published within the last minute

  @C15 @passing
  Scenario: the RSS pass is a query-less t=search over the configured categories, without a caps round-trip
    Given the indexer answers "t=search&q=" with the fixture "search_two_items"
    When the hunter pulls the RSS feed
    Then 2 releases are returned
    And release 1 has title "Lioness.S03E02.1080p.WEB-DL.h264-GRP"
    And release 2 has 0 seeders
    And the indexer received 1 request with "cat=2000"
    And the indexer received 0 requests with "t=caps"

  @C15 @passing
  Scenario: an authentication failure is an error, not an empty result
    Given the indexer answers "t=caps" with HTTP 401
    And a "movie" query for "Heat"
    When the hunter searches the indexer
    Then the search fails with an error containing "HTTP 401"

  @C15 @passing
  Scenario: a Cloudflare challenge page (HTTP 503) from a Torznab endpoint is an error after retries, never a silent empty feed
    Given the indexer answers "t=search&q=" with HTTP 503 and the fixture "challenge_html"
    When the hunter pulls the RSS feed
    Then the RSS pull fails with an error containing "HTTP 503"
    And the indexer received 4 requests with "t=search"

  @C15 @passing
  Scenario: a challenge page served with HTTP 200 parses as an empty feed rather than a crash
    Given the indexer answers "t=search&q=" with the fixture "challenge_html"
    When the hunter pulls the RSS feed
    Then 0 releases are returned

  @C15 @passing
  Scenario: a malformed XML body is reported as an error
    Given the indexer answers "t=search&q=" with the fixture "not_xml"
    When the hunter pulls the RSS feed
    Then the RSS pull fails

  @C15 @passing
  Scenario: the health check is a t=caps round-trip
    Given the indexer answers "t=caps" with the fixture "caps"
    When the indexer health check runs
    Then the health check passes
    And the indexer received 1 request with "t=caps"

  @C15 @passing @SKADI-T-0512
  Scenario: capabilities are cached across searches (Sonarr caches t=caps per indexer) instead of re-fetched every time
    Given the indexer answers "t=caps" with the fixture "caps"
    And the indexer answers "t=search" with the fixture "search_magnet"
    And a "movie" query for "Heat"
    When the hunter searches the indexer 3 times
    Then the indexer received 1 request with "t=caps"

  @C15 @passing
  Scenario: the domain served is derived from the configured categories (2000s movies, 3000s audiobooks)
    Then the indexer serves the "movie" domain
    And the indexer does not serve the "audiobook" domain
    And the indexer does not serve the "series" domain

  @C15 @passing @SKADI-T-0499
  Scenario: a Torznab indexer configured with TV categories serves the series domain (Sonarr's whole indexer path)
    Given a Torznab indexer configured with categories "5000,5040"
    Then the indexer serves the "series" domain
    And the indexer does not serve the "movie" domain

  @C15 @passing
  Scenario: a series query with a tvdb id issues t=tvsearch with season and episode params
    Given a Torznab indexer configured with categories "5000"
    And the indexer answers "t=caps" with the fixture "caps_tv"
    And the indexer answers "t=tvsearch" with the fixture "search_two_items"
    And a "series" query for "Lioness"
    And the query carries tvdb id 411301
    And the query asks for season 3 episode 2
    When the hunter searches the indexer
    Then 2 releases are returned
    And the indexer received 1 request with "tvdbid=411301"
    And the indexer received 1 request with "season=3"
    And the indexer received 1 request with "ep=2"
    And release 1 carries categories "5040"

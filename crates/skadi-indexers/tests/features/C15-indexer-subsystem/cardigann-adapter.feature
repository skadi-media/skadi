Feature: Native Cardigann trackers presented as skadi indexers
  crates/skadi-indexers/src/cardigann.rs wraps the skadi-cardigann engine: a
  per-indexer cookie jar, lazy login, tracker-category scoping per media kind, and
  the mapping of engine rows onto the Release contract. Prowlarr equivalent: the
  Cardigann indexer type (YAML definitions). The tracker is an in-process mock.

  @C15 @passing
  Scenario: a public JSON tracker search yields releases with magnet, size, seeders and date
    Given a cardigann indexer from the "public_json" definition
    And the tracker answers "/search" with the fixture "tracker_json_rows"
    And a "movie" query for "The Matrix"
    When the hunter searches the indexer
    Then 2 releases are returned
    And release 1 has title "The Matrix 1999 1080p BluRay x264"
    And release 1's fetch is a magnet containing "btih:AAAABBBB"
    And release 1 has size 1500000000
    And release 1 has 42 seeders
    And release 1 was published at "2023-11-14T22:13:20+00:00"
    And the tracker received 1 request with "q=The Matrix"

  @C15 @passing
  Scenario: the search is scoped to the tracker categories mapped to the requested media kind
    Given a cardigann indexer from the "public_json" definition
    And the tracker answers "/search" with the fixture "tracker_json_tv_rows"
    And a "series" query for "Lioness"
    When the hunter searches the indexer
    Then 1 release is returned
    And the tracker saw a request whose "cat" parameter contains "2"
    And the tracker saw no request whose "cat" parameter contains "1"

  @C15 @passing
  Scenario: the domain served follows the definition's category mappings
    Given a cardigann indexer from the "tv_only" definition
    Then the indexer serves the "series" domain
    And the indexer does not serve the "movie" domain
    And the indexer does not serve the "audiobook" domain

  @C15 @passing
  Scenario: capabilities expose the id params the definition's search modes accept
    Given a cardigann indexer from the "public_json" definition
    When the indexer capabilities are negotiated
    Then the capabilities advertise id params "imdb,tvdb"
    And the capabilities do support search

  @C15 @passing @SKADI-T-0498
  Scenario: an IMDb id reaches the tracker through the definition's .Query.IMDBID template (yts, torrentleech, 1337x use it)
    Given a cardigann indexer from the "public_json" definition
    And the tracker answers "/search" with the fixture "tracker_json_rows"
    And a "movie" query for "The Matrix" (1999)
    And the query carries imdb id "tt0133093"
    When the hunter searches the indexer
    Then the tracker saw a request whose "imdb" parameter contains "tt0133093"

  @C15 @passing @SKADI-T-0498
  Scenario: season and episode reach the tracker through .Query.Season / .Query.Ep
    Given a cardigann indexer from the "public_json" definition
    And the tracker answers "/search" with the fixture "tracker_json_tv_rows"
    And a "series" query for "Lioness"
    And the query asks for season 3 episode 2
    When the hunter searches the indexer
    Then the tracker saw a request whose "s" parameter contains "3"
    And the tracker saw a request whose "e" parameter contains "2"

  @C15 @bug @SKADI-T-0431
  Scenario: the RSS pass of a mixed movie+TV tracker is not scoped to movie categories only
    Given a cardigann indexer from the "public_json" definition
    And the tracker answers "/search" with the fixture "tracker_json_tv_rows"
    When the hunter pulls the RSS feed
    Then the tracker received 1 request with "q="
    And the tracker saw no request whose "cat" parameter contains "1"

  @C15 @passing @SKADI-T-0506
  Scenario: a release carries the Newznab category its tracker row mapped to, so the off-category gate can act
    Given a cardigann indexer from the "public_json" definition
    And the tracker answers "/search" with the fixture "tracker_json_rows"
    And a "movie" query for "The Matrix"
    When the hunter searches the indexer
    Then release 1 carries categories "2000"

  @C15 @passing @SKADI-T-0506
  Scenario: every title alias is searched, not only the first
    Given a cardigann indexer from the "public_json" definition
    And the tracker answers "/search" with the fixture "tracker_json_rows"
    And a "movie" query for "The Matrix"
    And the query also has the alias "Matrix"
    When the hunter searches the indexer
    Then the tracker received 2 requests to "/search"

  @C15 @passing
  Scenario: a relative .torrent download link is absolutised against the tracker site
    Given a cardigann indexer from the "torrent_link" definition
    And the tracker answers "/search" with the fixture "tracker_torrent_link_rows"
    And a "movie" query for "Heat"
    When the hunter searches the indexer
    Then 1 release is returned
    And release 1's fetch is a torrent URL containing "/dl/1.torrent"

  @C15 @passing
  Scenario: a detail-page-only release is resolved to a magnet at grab time by scraping the info hash
    Given a cardigann indexer from the "detail_hash" definition
    And the tracker answers "/search" with the fixture "tracker_detail_rows"
    And the tracker answers "/detail/1" with the fixture "tracker_detail_page"
    And a "audiobook" query for "Project Hail Mary"
    When the hunter searches the indexer
    Then 1 release is returned
    And release 1's fetch is a torrent URL containing "/detail/1"
    When the grab resolves release 1's fetch
    Then the resolved fetch is a magnet containing "btih:0123456789abcdef0123456789abcdef01234567"

  @C15 @passing
  Scenario: a detail page without an info hash fails the grab loudly instead of enqueuing the page
    Given a cardigann indexer from the "detail_hash" definition
    And the tracker answers "/search" with the fixture "tracker_detail_rows"
    And the tracker answers "/detail/1" with the fixture "tracker_detail_page_no_hash"
    And a "audiobook" query for "Project Hail Mary"
    When the hunter searches the indexer
    And the grab resolves release 1's fetch
    Then the grab fails with an error containing "did not yield a 40-char info-hash"

  @C15 @passing
  Scenario: a magnet passes through grab-time resolution untouched
    Given a cardigann indexer from the "public_json" definition
    And the tracker answers "/search" with the fixture "tracker_json_rows"
    And a "movie" query for "The Matrix"
    When the hunter searches the indexer
    And the grab resolves release 1's fetch
    Then the resolved fetch is unchanged

  @C15 @passing
  Scenario: a private tracker logs in once and carries the session cookie into every search
    Given a cardigann indexer from the "private_form" definition with username "alice" and password "hunter2"
    And the tracker requires a login session and its account page shows "logged in"
    And a "movie" query for "Some Movie"
    When the hunter searches the indexer 2 times
    Then 1 release is returned
    And release 1's fetch is a magnet containing "btih:DEADBEEF"
    And the tracker received 1 request to "/takelogin.php"
    And the tracker received 2 requests to "/search.php"

  @C15 @passing
  Scenario: a failed login (test selector absent) fails the search and the health check
    Given a cardigann indexer from the "private_form" definition with username "alice" and password "wrong"
    And the tracker requires a login session and its account page shows "logged out"
    And a "movie" query for "Some Movie"
    When the hunter searches the indexer
    Then the search fails with an error containing "login failed"
    When the indexer health check runs
    Then the health check fails with an error containing "login failed"

  @C15 @passing
  Scenario: the health check of a public tracker is a site reachability probe
    Given a cardigann indexer from the "public_json" definition
    And the tracker answers "/" with HTTP 500
    When the indexer health check runs
    Then the health check fails with an error containing "HTTP 500"

  @C15 @passing @SKADI-T-0496
  Scenario: a tracker that refuses every search path fails the search rather than reporting nothing found
    # A refused path is skipped so a multi-path definition survives one bad path
    # (1337x ships four). When every path was refused there is nothing left to
    # skip to, and returning an empty result told the caller "this tracker has
    # nothing for you" — which then recorded the indexer as healthy. That is how
    # a Cloudflare challenge with FlareSolverr down looked like an empty library.
    Given a cardigann indexer from the "public_json" definition
    And the tracker answers "/search" with HTTP 404
    And a "movie" query for "Nothing"
    When the hunter searches the indexer
    Then the search fails

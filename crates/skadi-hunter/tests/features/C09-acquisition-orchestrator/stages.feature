Feature: Acquisition orchestrator — the runner-free pipeline stages
  Each stage of the grab loop (search → snatch → monitor → import → notify)
  is a plain async function over the run state plus fakes for the
  capabilities it needs (REQ-ORCH.1–.8, NFR-ORCH.4).

  @C09 @passing
  Scenario: search fans out to every indexer serving the kind and de-duplicates
    Given an acquire run for a movie
    And an indexer serving movies with:
      | title                             |
      | Movie.2020.1080p.BluRay.x264-GRP  |
      | Movie.2020.720p.HDTV.x264-GRP     |
    And an indexer serving movies with:
      | title                             |
      | Movie.2020.1080p.BluRay.x264-GRP  |
    And an indexer that only serves audiobooks
    When the hunter searches
    Then 2 candidates are collected

  @C09 @passing
  Scenario: one failing indexer is tolerated
    Given an acquire run for a movie
    And an indexer serving movies that is down
    And an indexer serving movies with:
      | title                             |
      | Movie.2020.1080p.BluRay.x264-GRP  |
    When the hunter searches
    Then 1 candidates are collected

  @C09 @passing
  Scenario: every indexer failing surfaces the error for a retry
    Given an acquire run for a movie
    And an indexer serving movies that is down
    And an indexer serving movies that is down
    When the hunter searches
    Then the search fails mentioning "indexer down"

  @C09 @passing
  Scenario: no indexer serving the kind is a configuration error
    Given an acquire run for a movie
    And an indexer that only serves audiobooks
    When the hunter searches
    Then the search fails mentioning "no enabled indexer serves this media kind"

  @C09 @passing
  Scenario: snatch routes a torrent to the torrent client and records the handle
    Given the run chose the magnet release "Movie.2020.1080p.BluRay.x264-GRP"
    And a torrent client
    When the hunter snatches
    Then the run records a download handle
    And the client holds 1 transfer

  @C09 @passing
  Scenario: snatch is idempotent on a resumed run
    Given the run chose the magnet release "Movie.2020.1080p.BluRay.x264-GRP"
    And a torrent client
    When the hunter snatches
    And the hunter snatches again
    Then the client holds 1 transfer

  @C09 @passing
  Scenario: snatch needs a client speaking the release's protocol
    Given the run chose the nzb release "Movie.2020.1080p.BluRay.x264-GRP"
    And a torrent client
    When the hunter snatches
    Then the stage fails mentioning "no enabled downloader supports protocol Usenet"
    And the client holds 0 transfers

  @C09 @passing
  Scenario: monitor follows the transfer to completion and records its files
    Given the run chose the magnet release "Movie.2020.1080p.BluRay.x264-GRP"
    And a torrent client that will report:
      | status           |
      | Queued           |
      | Downloading 40%  |
      | Completed        |
    And the transfer was handed to the client
    When the hunter monitors with a budget of 5 polls
    Then the run records 1 completed file

  @C09 @passing
  Scenario: an exhausted poll budget asks Cloacina to retry the watch
    Given the run chose the magnet release "Movie.2020.1080p.BluRay.x264-GRP"
    And a torrent client that will report:
      | status           |
      | Downloading 40%  |
    And the transfer was handed to the client
    When the hunter monitors with a budget of 1 poll
    Then monitor asks to be retried later

  @C09 @passing
  Scenario: a failed transfer is a hard failure, and the transfer can be dropped
    Given the run chose the magnet release "Movie.2020.1080p.BluRay.x264-GRP"
    And a torrent client that will report:
      | status  |
      | Failed  |
    And the transfer was handed to the client
    When the hunter monitors with a budget of 3 polls
    Then monitor reports a hard download failure
    When the transfer is cancelled
    Then the client removed 1 transfer

  @C09 @passing
  Scenario: import places the completed files through the domain matcher
    Given the completed download holds 2 media files
    When the hunter imports into the library
    Then 2 files are placed in the library
    And a second import of the same files counts as already imported

  @C09 @passing
  Scenario: an import that places nothing is a failure, not a success
    Given the completed download holds 1 media file
    When the hunter imports with a matcher that places nothing
    Then the import fails because nothing was placed

  @C09 @passing
  Scenario: Imported fans out only to notifiers that want it, failures are non-fatal
    Given a notifier subscribed to "Imported, Grabbed"
    And a notifier subscribed to "Failed"
    And a notifier whose webhook always fails
    When the hunter announces the import
    Then the notification stage succeeds
    And notifier 1 received "Imported"
    And notifier 2 received nothing

  @C09 @passing
  Scenario: Grabbed is announced at the snatch boundary
    Given a notifier subscribed to "Grabbed"
    When the hunter announces the grab
    Then notifier 1 received "Grabbed"

Feature: The built-in skadi downloader — the daemon's side of the downloads-queue contract
  crates/skadi-downloaders/src/db.rs enqueues a `downloads` row per grab and maps
  the row the VPN-isolated worker maintains back onto DownloadStatus for the
  hunter. Sonarr equivalent: a download client whose queue the app polls
  (status → Queued/Downloading/Completed/Failed, remove with/without data).

  Background:
    Given the built-in skadi downloader over a fresh queue with incomplete dir "/data/incomplete" and complete dir "/data/complete"

  @C16 @passing
  Scenario: a grab enqueues a queued row carrying the source, category and the configured dirs
    Given a magnet release "The Matrix" with info hash "abc"
    When the hunter adds the release under category 2000
    Then a queued job exists for the handle with source "magnet:?xt=urn:btih:abc&dn=x" under category "2000"
    And the job carries incomplete dir "/data/incomplete" and complete dir "/data/complete"
    And the job's acquirable ref is the release title "The Matrix"
    When the hunter reads the status
    Then the status is queued

  @C16 @passing
  Scenario: a .torrent URL release is enqueued as-is for the worker to resolve
    Given a .torrent URL release "Heat" at "http://gluetun:9696/1/download?apikey=k&link=AAAA"
    When the hunter adds the release under category 2000
    Then a queued job exists for the handle with source "http://gluetun:9696/1/download?apikey=k&link=AAAA" under category "2000"

  @C16 @passing
  Scenario: an NZB release is refused (torrents only)
    Given an NZB release "Heat"
    When the hunter adds the release under category 2000
    Then the add is rejected with a validation error mentioning "NZB"

  @C16 @passing
  Scenario: worker progress is surfaced as a downloading fraction
    Given a magnet release "The Matrix" with info hash "abc"
    When the hunter adds the release under category 2000
    And the worker claims the job as "w1"
    And the worker reports 250 of 1000 bytes with 3 peers
    And the hunter reads the status
    Then the status is downloading at 25%

  @C16 @passing
  Scenario: before the worker knows the total size the fraction is zero, never a division error
    Given a magnet release "The Matrix" with info hash "abc"
    When the hunter adds the release under category 2000
    And the worker claims the job as "w1"
    And the worker reports 0 of 0 bytes with 0 peers
    And the hunter reads the status
    Then the status is downloading at 0%

  @C16 @passing
  Scenario: completion carries the worker's absolute file paths for the importer
    Given a magnet release "The Matrix" with info hash "abc"
    When the hunter adds the release under category 2000
    And the worker claims the job as "w1"
    And the worker marks the job complete with files "/data/complete/The Matrix/movie.mkv, /data/complete/The Matrix/movie.srt"
    And the hunter reads the status
    Then the status is completed with files "/data/complete/The Matrix/movie.mkv, /data/complete/The Matrix/movie.srt"

  @C16 @passing
  Scenario: a seed-limit-stopped transfer is still a completed download (the files are on disk)
    Given a magnet release "The Matrix" with info hash "abc"
    When the hunter adds the release under category 2000
    And the worker claims the job as "w1"
    And the worker marks the job complete with files "/data/complete/x.mkv"
    And the worker marks the job seeded
    And the hunter reads the status
    Then the status is completed with files "/data/complete/x.mkv"

  @C16 @passing
  Scenario: a worker error surfaces as a failure with the worker's reason
    Given a magnet release "The Matrix" with info hash "abc"
    When the hunter adds the release under category 2000
    And the worker claims the job as "w1"
    And the worker marks the job failed with "add failed: error decoding torrent"
    And the hunter reads the status
    Then the status is failed with a reason containing "error decoding torrent"

  @C16 @passing
  Scenario: an operator-paused or stalled transfer stays in flight from the hunter's view
    Given a magnet release "The Matrix" with info hash "abc"
    When the hunter adds the release under category 2000
    And the worker claims the job as "w1"
    And the worker reports 500 of 1000 bytes with 0 peers
    And the worker flags the job stalled
    And the hunter reads the status
    Then the status is downloading at 50%
    And the download is still in flight from the hunter's view
    When the operator pauses the job
    And the hunter reads the status
    Then the download is still in flight from the hunter's view

  @C16 @passing
  Scenario: removing a download flags the row for the worker, with or without deleting data
    Given a magnet release "Dune" with info hash "abc"
    When the hunter adds the release under category 2000
    And the hunter removes the download deleting its data
    Then the queue holds a remove request for the job with delete_data true
    When the hunter reads the status
    Then the status is failed with a reason containing "download removed"
    And the download is no longer in flight from the hunter's view

  @C16 @passing
  Scenario: a torn-down (removed) row is terminal
    Given a magnet release "Dune" with info hash "abc"
    When the hunter adds the release under category 2000
    And the hunter removes the download keeping its data
    Then the queue holds a remove request for the job with delete_data false
    When the worker marks the job removed
    And the hunter reads the status
    Then the status is failed with a reason containing "download removed"

  @C16 @passing
  Scenario: an unknown handle is not found
    When the hunter reads the status of the unknown handle "nope"
    Then the status lookup reports not found

  @C16 @passing
  Scenario: the connectivity test is a queue round-trip
    When the downloader connectivity test runs
    Then the connectivity test passes

  @C16 @passing @SKADI-T-0394
  Scenario: a claimed transfer that the client has not yet made live (checking / queued in librqbit) is not reported as downloading at 0%
    Given a magnet release "The Matrix" with info hash "abc"
    When the hunter adds the release under category 2000
    And the worker claims the job as "w1"
    And the worker reports 0 of 65000000000 bytes with 0 peers
    And the worker reports the client state "initializing"
    And the hunter reads the status
    Then the status is queued

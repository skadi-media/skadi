Feature: Acquisition orchestrator — run lifecycle through the step bodies
  The Cloacina step bodies write status at task boundaries (REQ-ORCH.9),
  keep transient failures retryable and blocklist only release-at-fault
  failures (REQ-ORCH.16), and never let a duplicate run demote an import.

  Background:
    Given a registered movie domain with an in-memory status sink

  @C09 @passing @serial
  Scenario: a successful snatch writes Snatched, announces Grabbed once and explains the grab
    Given a notifier subscribed to "Grabbed, Imported"
    And a registered movie domain with an in-memory status sink
    And a run "run-a" for "ed-1" that chose "Movie.2020.1080p.BluRay.x264-GRP"
    And run "run-a" was launched by the sweep
    When run "run-a" snatches
    And run "run-a" snatches
    Then the step succeeds
    And the client holds 1 transfer
    And the status of "ed-1" is Snatched
    And notifier 1 saw a Grabbed event exactly once
    And the trace for "ed-1" has event "snatched"
    And the decision history for "ed-1" explains the grab with a release key
    And the tracker shows "ed-1" at stage "snatching"

  @C09 @passing @serial
  Scenario: a client refusing the add is a transient failure — retry later, no blocklist
    Given a torrent client that refuses every add
    And a registered movie domain with an in-memory status sink
    And a run "run-a" for "ed-1" that chose "Movie.2020.1080p.BluRay.x264-GRP"
    When run "run-a" snatches
    Then the step fails mentioning "client refused the add"
    And the status of "ed-1" is Failed with a retry backoff of about 30 minutes
    And "Movie.2020.1080p.BluRay.x264-GRP" is not blocklisted
    And "ed-1" is no longer tracked

  @C09 @passing @serial
  Scenario: a first-acquire run for an item already in the library ends without grabbing (SKADI-T-0388)
    Given a run "run-a" for "ed-1" that chose "Movie.2020.1080p.BluRay.x264-GRP"
    And "ed-1" is already Imported
    When run "run-a" snatches
    Then the step succeeds
    And run "run-a" ended superseded
    And the client holds 0 transfers
    And the status of "ed-1" is Imported
    And the trace for "ed-1" has event "superseded"

  @C09 @passing @serial
  Scenario: an upgrade run may replace an imported file
    Given a run "run-a" for "ed-1" that chose "Movie.2020.2160p.BluRay.x265-GRP"
    And run "run-a" is an upgrade run
    And "ed-1" is already Imported
    When run "run-a" snatches
    Then the step succeeds
    And the client holds 1 transfer
    And run "run-a" is not flagged terminal

  @C09 @passing @serial
  Scenario: a manual grab may replace an imported file
    Given a run "run-a" for "ed-1" that chose "Movie.2020.2160p.BluRay.x265-GRP"
    And run "run-a" is a manual grab
    And "ed-1" is already Imported
    When run "run-a" snatches
    Then the step succeeds
    And the client holds 1 transfer

  @C09 @passing @serial
  Scenario: a live transfer reports progress and keeps the run alive
    Given a torrent client that will report:
      | status           |
      | Downloading 42%  |
    And a registered movie domain with an in-memory status sink
    And a run "run-a" for "ed-1" that chose "Movie.2020.1080p.BluRay.x264-GRP"
    When run "run-a" snatches
    And run "run-a" monitors
    Then the step fails mentioning "Network"
    And the status of "ed-1" is Downloading at 42 percent
    And the transfer watch for "ed-1" shows best progress 42 percent
    And the tracker shows "ed-1" at stage "downloading"
    And run "run-a" is not flagged terminal

  @C09 @passing @serial
  Scenario: a hard download failure ends the run, blocklists the release and drops the transfer
    Given a torrent client that will report:
      | status  |
      | Failed  |
    And a registered movie domain with an in-memory status sink
    And a run "run-a" for "ed-1" that chose "Movie.2020.1080p.BluRay.x264-GRP"
    When run "run-a" snatches
    And run "run-a" monitors
    Then the step succeeds
    And run "run-a" is flagged terminal
    # Immediately retryable since SKADI-T-0529 (Sonarr "Redownload Failed"): the
    # release is blocklisted on the line below, so the next sweep cannot choose it
    # again and the backoff has nothing left to protect against — its purpose was
    # to avoid hammering this same bad release.
    And the status of "ed-1" is Failed and immediately retryable
    And "Movie.2020.1080p.BluRay.x264-GRP" is blocklisted
    And the blocklist entry for "Movie.2020.1080p.BluRay.x264-GRP" records reason "DownloadFailed"
    And the trace for "ed-1" has event "download_failed"
    When run "run-a" imports
    And run "run-a" notifies
    Then the step succeeds
    And the status of "ed-1" is Failed
    And "ed-1" is no longer tracked

  @C09 @passing @serial
  Scenario: a completed transfer is imported, announced and released from the tracker
    Given a notifier subscribed to "Imported"
    And a torrent client that will report:
      | status     |
      | Completed  |
    And a registered movie domain with an in-memory status sink
    And a run "run-a" for "item-1" that chose "Movie.2020.1080p.BluRay.x264-GRP"
    And run "run-a" completed with a media file on disk
    When run "run-a" imports
    And run "run-a" notifies
    Then the step succeeds
    And the status of "item-1" is Imported
    And the trace for "item-1" has event "imported"
    And notifier 1 received "Imported"
    And "item-1" is no longer tracked

  @C09 @passing @serial
  Scenario: an import that places nothing blocklists the release
    Given the domain importer matches nothing
    And a run "run-a" for "ed-9" that chose "Movie.2020.1080p.BluRay.x264-GRP"
    And run "run-a" completed with a media file on disk
    When run "run-a" imports
    Then the step fails mentioning "no placed files"
    And "Movie.2020.1080p.BluRay.x264-GRP" is blocklisted
    And the blocklist entry for "Movie.2020.1080p.BluRay.x264-GRP" records reason "ImportFailed"
    # Immediately retryable since SKADI-T-0529 — see the note above.
    And the status of "ed-9" is Failed and immediately retryable
    And the trace for "ed-9" has event "import_failed"

  @C09 @passing @serial
  Scenario: two workflows replayed after a restart snatch exactly once (SKADI-T-0388)
    Given a run "run-a" for "ed-1" that chose "Movie.2020.1080p.BluRay.x264-GRP"
    And a run "run-b" for "ed-1" that chose "Movie.2020.1080p.BluRay.x264-GRP"
    When run "run-a" snatches
    And run "run-b" snatches
    Then the client holds 1 transfer
    And run "run-b" ended superseded
    And the tracker entry for "ed-1" belongs to run "run-a"
    And the tracker entry for "ed-1" is adopted
    And the sweep cannot start another run for "ed-1"
    And the status of "ed-1" is Snatched

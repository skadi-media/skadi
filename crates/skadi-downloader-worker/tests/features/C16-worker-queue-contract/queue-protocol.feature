Feature: The worker's side of the downloads-queue contract — claim, lease, progress, terminal states, removals, restarts
  crates/skadi-downloader-worker/src/lib.rs drives the `downloads` table through
  skadi-store's DownloadJobRepo: it claims queued rows, heartbeats a lease while
  tracking, writes progress/completion/errors, and actions pause/removal
  requests. These scenarios exercise that protocol on a throw-away SQLite queue
  (the librqbit session is out of scope — network).

  Background:
    Given an empty downloads queue

  @C16 @passing
  Scenario: the oldest queued row is claimed first, flipped to downloading for the claiming worker
    Given the daemon enqueued "first" from "magnet:?xt=urn:btih:0000000000000000000000000000000000000001"
    And the daemon enqueued "second" from "magnet:?xt=urn:btih:0000000000000000000000000000000000000002"
    When worker "w1" claims the next job
    Then the claim yields "first" now downloading for worker "w1"
    When worker "w1" claims the next job
    Then the claim yields "second" now downloading for worker "w1"
    When worker "w1" claims the next job
    Then the claim yields nothing
    And 2 transfers count as active

  @C16 @passing
  Scenario: a live transfer heartbeats its lease each tick and is never swept
    Given the daemon enqueued "job" from "magnet:?xt=urn:btih:0000000000000000000000000000000000000001"
    When worker "w1" claims the next job
    And the worker heartbeats "job" with a lease of 60 seconds
    Then "job" has a lease in the future
    When the worker sweeps lapsed leases
    Then 0 jobs were re-queued
    And "job" still belongs to worker "w1"

  @C16 @passing
  Scenario: a transfer whose worker died (lapsed lease) is re-queued for another claim, keeping what it knew
    Given the daemon enqueued "job" from "magnet:?xt=urn:btih:0000000000000000000000000000000000000001"
    When worker "w1" claims the next job
    And the worker reports "job" at 100 of 1000 bytes with info hash "0000000000000000000000000000000000000001" and 2 peers
    And the worker heartbeats "job" with a lease of -1 seconds
    And the worker sweeps lapsed leases
    Then 1 job was re-queued
    And "job" is queued
    And "job" has no worker and no lease
    And "job" keeps info hash "0000000000000000000000000000000000000001" and 100 progress bytes
    When worker "w2" claims the next job
    Then the claim yields "job" now downloading for worker "w2"

  @C16 @passing
  Scenario: a freshly claimed row whose first heartbeat has not landed is left alone by the sweep
    Given the daemon enqueued "job" from "magnet:?xt=urn:btih:0000000000000000000000000000000000000001"
    When worker "w1" claims the next job
    And the worker sweeps lapsed leases
    Then 0 jobs were re-queued
    And "job" is downloading

  @C16 @passing
  Scenario: on startup a worker re-queues every transfer left downloading by its previous instance, but not finished ones
    Given the daemon enqueued "inflight" from "magnet:?xt=urn:btih:0000000000000000000000000000000000000001"
    And the daemon enqueued "done" from "magnet:?xt=urn:btih:0000000000000000000000000000000000000002"
    When worker "old" claims the next job
    And worker "old" claims the next job
    And the worker marks "done" complete with files "/data/complete/done/x.mkv"
    And a fresh worker starts up and re-queues orphaned transfers
    Then 1 job was re-queued
    And "inflight" is queued
    And "inflight" has no worker and no lease
    And "done" is completed

  @C16 @passing
  Scenario: progress ticks record bytes, the info hash and live peer metrics on the row
    Given the daemon enqueued "job" from "magnet:?xt=urn:btih:0000000000000000000000000000000000000001"
    When worker "w1" claims the next job
    And the worker reports "job" at 250 of 1000 bytes with info hash "0000000000000000000000000000000000000001" and 4 peers
    Then "job" is downloading
    And "job" keeps info hash "0000000000000000000000000000000000000001" and 250 progress bytes
    And "job" shows 4 peers and a download speed

  @C16 @passing
  Scenario: completion stamps the files and the seed-start time; a seed limit then makes the row terminal
    Given the daemon enqueued "job" from "magnet:?xt=urn:btih:0000000000000000000000000000000000000001"
    When worker "w1" claims the next job
    And the worker marks "job" complete with files "/data/complete/job/a.mkv, /data/complete/job/a.srt"
    Then "job" is completed
    And "job" has files "/data/complete/job/a.mkv, /data/complete/job/a.srt" and a completion time
    And 0 transfers count as active
    When the worker marks "job" seeded
    Then "job" is seeded

  @C16 @passing
  Scenario: a failed add or transfer records the reason on the row
    Given the daemon enqueued "job" from "http://tracker.test/dl/1.torrent"
    When worker "w1" claims the next job
    And the worker marks "job" failed with "add failed: error decoding torrent"
    Then "job" is error
    And "job" carries the error "add failed: error decoding torrent"

  @C16 @passing
  Scenario: the stall flag flips a downloading row to stalled and back, and only from those states
    Given the daemon enqueued "job" from "magnet:?xt=urn:btih:0000000000000000000000000000000000000001"
    When worker "w1" claims the next job
    And the worker flags "job" stalled
    Then "job" is stalled
    When the worker flags "job" unstalled
    Then "job" is downloading
    When the worker marks "job" complete with files "/data/complete/job/a.mkv"
    And the worker flags "job" stalled
    Then "job" is completed

  @C16 @passing
  Scenario: an operator pause takes the row out of the claim pool until it is resumed
    Given the daemon enqueued "job" from "magnet:?xt=urn:btih:0000000000000000000000000000000000000001"
    When the operator pauses "job"
    Then "job" is paused
    When worker "w1" claims the next job
    Then the claim yields nothing
    When the operator resumes "job"
    Then "job" is queued
    And "job" has no worker and no lease
    When worker "w1" claims the next job
    Then the claim yields "job" now downloading for worker "w1"

  @C16 @passing
  Scenario: a removal request is handed to the worker with the delete-data flag, then torn down
    Given the daemon enqueued "keep" from "magnet:?xt=urn:btih:0000000000000000000000000000000000000001"
    And the daemon enqueued "purge" from "magnet:?xt=urn:btih:0000000000000000000000000000000000000002"
    When worker "w1" claims the next job
    And the daemon requests removal of "keep" keeping its data
    And the daemon requests removal of "purge" deleting its data
    Then the removal sweep lists "keep" with delete_data false
    And the removal sweep lists "purge" with delete_data true
    And "keep" is remove_requested
    When the worker tears down "keep"
    And the worker tears down "purge"
    Then "keep" is removed
    And "purge" is removed
    And the removal sweep is empty

  @C16 @passing @SKADI-T-0513
  Scenario: a stalled transfer still occupies a concurrency slot (it is still loaded in the client)
    Given the daemon enqueued "a" from "magnet:?xt=urn:btih:0000000000000000000000000000000000000001"
    And the daemon enqueued "b" from "magnet:?xt=urn:btih:0000000000000000000000000000000000000002"
    When worker "w1" claims the next job
    And worker "w1" claims the next job
    And the worker flags "a" stalled
    Then 2 transfers count as active

  @C16 @passing @SKADI-T-0394
  Scenario: the worker records the client's transfer state so the daemon can tell "checking / queued in the client" from "live with no peers"
    Given the daemon enqueued "job" from "magnet:?xt=urn:btih:0000000000000000000000000000000000000001"
    When worker "w1" claims the next job
    And the worker reports "job" at 0 of 65000000000 bytes with info hash "0000000000000000000000000000000000000001" and 0 peers
    Then "job" records the client state "initializing"

  @C16 @passing @SKADI-T-0489
  Scenario: a session restore that outruns max_active re-queues the excess
    # librqbit restarts every persisted torrent regardless of our cap, which is
    # how a 4 GB NAS came up running 38 transfers (SKADI-T-0392).
    Given the daemon enqueued "a" from "magnet:?xt=urn:btih:0000000000000000000000000000000000000001"
    And the daemon enqueued "b" from "magnet:?xt=urn:btih:0000000000000000000000000000000000000002"
    And the daemon enqueued "c" from "magnet:?xt=urn:btih:0000000000000000000000000000000000000003"
    When worker "w1" claims the next job
    And worker "w1" claims the next job
    And worker "w1" claims the next job
    Then 3 transfers count as active
    When the worker enforces a max_active of 2
    Then 2 transfers count as active
    And "c" is queued again

  @C16 @passing @SKADI-T-0489
  Scenario: enforcing the cap keeps the oldest claims, matching the queue's FIFO order
    Given the daemon enqueued "a" from "magnet:?xt=urn:btih:0000000000000000000000000000000000000001"
    And the daemon enqueued "b" from "magnet:?xt=urn:btih:0000000000000000000000000000000000000002"
    When worker "w1" claims the next job
    And worker "w1" claims the next job
    And the worker enforces a max_active of 1
    Then "a" is downloading
    And "b" is queued again

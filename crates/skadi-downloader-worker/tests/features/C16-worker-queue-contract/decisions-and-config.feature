Feature: The worker's pure decisions and its configuration plane
  Seed-policy verdicts (SKADI-T-0209/0210/0215), stall detection (T-0213), the
  claim budget (T-0212), bandwidth caps (T-0211), completed-file placement, and
  the `worker.*` keys on the shared config table (T-0103, hot-reloaded per tick
  per T-0291). Sonarr equivalent: download-client seed ratio/time + "remove
  completed", plus the max-active-torrents and speed limits a torrent client
  conventionally offers.

  @C16 @passing
  Scenario Outline: a seed stops or is removed once its ratio or time limit is reached
    Given a seed policy of ratio <ratio> and <mins> minutes with action "<action>"
    When a seed has uploaded <up> of <down> downloaded bytes after <secs> seconds
    Then the seed verdict is "<verdict>"

    Examples:
      | ratio | mins | action | up   | down | secs  | verdict  |
      | 2.0   | 0    | stop   | 1999 | 1000 | 99999 | continue |
      | 2.0   | 0    | stop   | 2000 | 1000 | 0     | stop     |
      | 0     | 60   | stop   | 0    | 1000 | 3599  | continue |
      | 0     | 60   | remove | 0    | 1000 | 3600  | remove   |
      | 1.0   | 60   | remove | 500  | 1000 | 3600  | remove   |
      | 0     | 0    | remove | 9999 | 1    | 99999 | continue |
      | 1.0   | 0    | stop   | 5000 | 0    | 99999 | continue |

  @C16 @passing
  Scenario: a download category overrides the global seed policy per axis, with 0 meaning unlimited
    Given a seed policy of ratio 2.0 and 60 minutes with action "stop"
    And the category overrides ratio "0" time "" action "remove"
    Then the effective policy is ratio unlimited and time 3600 seconds
    When a seed has uploaded 0 of 1000 downloaded bytes after 3600 seconds
    Then the seed verdict is "remove"

  @C16 @passing
  Scenario: a transfer is stalled only with no peers, no progress for the timeout, and a timeout configured
    Then a transfer with 0 peers and 600 seconds without progress under a 600 second stall timeout is stalled
    And a transfer with 1 peers and 600 seconds without progress under a 600 second stall timeout is not stalled
    And a transfer with 0 peers and 599 seconds without progress under a 600 second stall timeout is not stalled
    And a transfer with 0 peers and 99999 seconds without progress under a 0 second stall timeout is not stalled

  @C16 @passing
  Scenario: the per-tick claim budget is the cap minus the active count
    Then with 3 active transfers and a cap of 5 the worker claims at most 2 more
    And with 5 active transfers and a cap of 5 the worker claims at most 0 more
    And with 7 active transfers and a cap of 5 the worker claims at most 0 more
    And with 100 active transfers and no cap the worker claims without bound

  @C16 @passing
  Scenario: a bandwidth cap of 0 means unlimited and large caps are clamped to the limiter's range
    Then a configured cap of 0 bytes per second becomes unlimited
    And a configured cap of 1048576 bytes per second becomes 1048576
    And a configured cap of 99999999999 bytes per second becomes 4294967295

  @C16 @passing
  Scenario: finished files are mirrored from the incomplete tree into the complete tree
    Then the file "/data/incomplete/Heat" with components "Heat.1995/Heat.mkv" resolves to "/data/incomplete/Heat/Heat.1995/Heat.mkv"
    When the finished file "/data/incomplete/Heat/Heat.mkv" is mapped from "/data/incomplete" to "/data/complete"
    Then the mapped path is "/data/complete/Heat/Heat.mkv"
    When the finished file "/elsewhere/Heat.mkv" is mapped from "/data/incomplete" to "/data/complete"
    Then the file is left where it is because it is outside the incomplete tree

  @C16 @passing
  Scenario: the worker resolves its settings from the shared config table with registry defaults
    Given the shared config table holds:
      | worker.download_dir        | /mnt/dl    |
      | worker.watch_dir           | /mnt/watch |
      | worker.max_active          | 6          |
      | worker.stall_timeout_secs  | 900        |
      | worker.lease_secs          | 120        |
      | worker.seed_ratio          | 1.5        |
      | worker.seed_time_mins      | 720        |
      | worker.seed_action         | remove     |
    Then the worker downloads into "/mnt/dl" and watches "/mnt/watch"
    And the worker caps 6 active transfers, stalls after 900 seconds and leases for 120 seconds
    And the worker seed policy is ratio 1.5, 720 minutes, action "remove"

  @C16 @passing
  Scenario: an empty config table yields the standalone defaults
    Given an empty shared config table
    Then the worker downloads into "/data/downloads/complete" and watches ""
    And the worker defaults to polling every 3s, ticking every 1s, ports 16881-16891, unlimited download, upload capped at 500000 bytes per second and no metadata timeout

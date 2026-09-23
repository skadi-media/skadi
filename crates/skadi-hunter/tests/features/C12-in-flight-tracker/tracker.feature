Feature: In-flight tracker — the live queue behind /activity
  A process-global registry of acquire runs keyed by acquirable: the sweep
  claims a slot per item (REQ-QUEUE.1), steps advance the stage and record
  what was chosen (REQ-QUEUE.2/.6), terminal runs leave (REQ-QUEUE.3), and
  workflows replayed after a restart are adopted back (REQ-QUEUE.12).

  Background:
    Given an empty in-flight tracker

  @C12 @passing @serial
  Scenario: a run is visible while in flight and gone when finished
    When the sweep starts run "r1" for "ed-1"
    Then the claim succeeds
    And the activity view lists 1 run
    And the activity view shows "ed-1" at stage "running"
    When the run for "ed-1" finishes
    Then the activity view lists 0 runs

  @C12 @passing @serial
  Scenario: a second claim for the same acquirable is refused until the first finishes
    Given the sweep started run "r1" for "ed-1"
    When the sweep starts run "r2" for "ed-1"
    Then the claim is refused because a run is already in flight
    When the run for "ed-1" finishes
    And the sweep starts run "r3" for "ed-1"
    Then the claim succeeds

  @C12 @passing @serial
  Scenario: the stage, chosen release, candidate count and decision are shown live
    Given the sweep started run "r1" for "ed-1"
    When run "r1" advances "ed-1" to stage "deciding"
    And "Movie.2020.1080p.BluRay.x264-GRP" is recorded as chosen for "ed-1" out of 12 candidates
    And the decision "Upgrade" is recorded for "ed-1"
    And run "r1" advances "ed-1" to stage "downloading"
    Then the activity view shows "ed-1" at stage "downloading"
    And the activity view shows "ed-1" chose "Movie.2020.1080p.BluRay.x264-GRP" of 12 with decision "Upgrade"

  @C12 @passing @serial
  Scenario: a workflow replayed after a restart is adopted and blocks the sweep
    When a replayed workflow "old-run" enters stage "downloading" for "ed-1"
    Then the workflow is adopted by the tracker
    And the activity view shows "ed-1" at stage "downloading"
    When the sweep starts run "r2" for "ed-1"
    Then the claim is refused because a run is already in flight

  @C12 @passing @serial
  Scenario: a run recognises its own tracker entry and a sibling's as foreign
    Given a replayed workflow "run-a" entered stage "snatching" for "ed-1"
    When a replayed workflow "run-a" enters stage "downloading" for "ed-1"
    Then the workflow is recognised by the tracker
    When a replayed workflow "run-b" enters stage "snatching" for "ed-1"
    Then the workflow is foreign by the tracker
    And the tracker still holds "ed-1" for run "run-a"

  @C12 @passing @serial
  Scenario: a legacy workflow with no run id is treated as the owner
    Given a replayed workflow "run-a" entered stage "snatching" for "ed-1"
    When a legacy workflow with no run id enters stage "downloading" for "ed-1"
    Then the workflow is recognised by the tracker

  @C12 @passing @serial
  Scenario: finishing an adopted run only removes its own entry
    Given a replayed workflow "run-a" entered stage "downloading" for "ed-1"
    When replayed workflow "run-b" finishes "ed-1"
    Then the tracker still holds "ed-1" for run "run-a"
    When replayed workflow "run-a" finishes "ed-1"
    Then "ed-1" is not in flight

  @C12 @passing @serial
  Scenario: owned and recently-seen adopted runs are never expired
    Given the sweep started run "r1" for "ed-1"
    And a replayed workflow "run-a" entered stage "downloading" for "ed-2"
    When the sweep expires adopted runs idle for more than 2 hours
    Then nothing was expired
    And the activity view lists 2 runs

  @C12 @passing @serial
  Scenario: progress heartbeats are throttled and best progress never drops
    Given a replayed workflow "run-a" entered stage "downloading" for "ed-1"
    When monitor observes 10 percent for "ed-1"
    Then the progress is flushed to the status sink
    When monitor observes 10 percent for "ed-1"
    Then the progress is throttled
    When monitor observes 12 percent for "ed-1"
    Then the progress is flushed to the status sink
    When monitor observes 3 percent for "ed-1"
    Then the transfer watch for "ed-1" has best progress 12 percent

  @C12 @gap @serial
  Scenario: a queue row carries the release size and time left
    Given the sweep started run "r1" for "ed-1"
    When run "r1" advances "ed-1" to stage "downloading"
    Then the activity view entry for "ed-1" carries the release size and ETA

  @C12 @gap @serial
  Scenario: a queue row carries the item's title
    Given the sweep started run "r1" for "ed-1"
    Then the activity view entry for "ed-1" carries a human-readable title

  @C12 @gap @serial
  Scenario: the live queue can be filtered by kind and stage
    Given the sweep started run "r1" for "ed-1"
    Then the activity view can be filtered to "movie" runs

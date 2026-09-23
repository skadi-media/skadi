Feature: History — what the hunter records durably
  The hunter writes two durable records: a per-step trace stream
  (`trace_events`: candidates found, decision, snatched, download/import
  failures, imports) and the grab-time `decision_history` explaining why a
  release was chosen. The coarse `acquisition_history` rows are written by
  the domains' status sinks (P2/P5). The worker purges history older than a
  fixed 90 days each tick.

  @C13 @passing @serial
  Scenario: a trace event is recorded against the acquirable in its domain
    Given a registered tv domain with an in-memory status sink
    When the hunter emits a "candidates_found" trace for "S03E01" at stage "searching"
    And the hunter emits a "decision" trace for "S03E01" at stage "deciding"
    Then the newest trace for "S03E01" is "decision" at stage "deciding" in domain "tv"
    And the trace stream lists 2 events newest first

  @C13 @passing
  Scenario: tracing without a registered domain never fails the pipeline
    Given no domain services are registered
    Then emitting a trace is a harmless no-op

  @C13 @passing @serial
  Scenario: the grab is explained in the decision history with its release key
    Given a registered movie domain with an in-memory status sink
    And a run "run-a" for "ed-1" that chose "Movie.2020.1080p.BluRay.x264-GRP"
    When run "run-a" snatches
    Then the decision history for "ed-1" explains the grab with a release key
    And the decision history for "ed-1" records kind "movie"

  @C13 @passing @SKADI-T-0433 @serial
  Scenario: a TV grab is recorded under the tv domain, not movie
    Given a registered tv domain with an in-memory status sink
    And a run "run-a" for "S03E01" that chose "Show.S03E01.1080p.WEB-DL.x264-GRP"
    When run "run-a" snatches
    Then the decision history for "S03E01" records kind "tv"

  @C13 @passing @serial
  Scenario: a superseded run leaves no decision row
    Given a registered movie domain with an in-memory status sink
    And a run "run-a" for "ed-1" that chose "Movie.2020.1080p.BluRay.x264-GRP"
    And "ed-1" is already Imported
    When run "run-a" snatches
    Then the decision history for "ed-1" is empty

  @C13 @passing @serial @SKADI-T-0440
  Scenario: history retention is an operator setting
    Given a registered movie domain with an in-memory status sink
    Then the hunter's history retention is operator-configurable

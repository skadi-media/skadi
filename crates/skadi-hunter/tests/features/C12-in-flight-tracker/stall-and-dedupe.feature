Feature: In-flight tracker — stalled transfers and duplicate grabs
  The 2026-09-06 incident: queued transfers were blocklisted as stalled
  (SKADI-T-0394) and the same season pack was sent to the client twice
  (SKADI-T-0402). Sonarr's stalled rule only counts time a download is
  actually live in the client, and a release already in the queue is never
  grabbed again.

  Background:
    Given a registered tv domain with an in-memory status sink

  @C12 @passing @SKADI-T-0394 @serial
  Scenario: a transfer merely queued in the client does not start the stall clock (SKADI-T-0394)
    Given a run "run-a" for "S03E01" that chose "Show.S03E01.1080p.WEB-DL.x264-GRP"
    When run "run-a" snatches
    And run "run-a" monitors
    Then the step fails mentioning "Network"
    And the stall clock for "S03E01" has not started

  @C12 @bug @SKADI-T-0402 @serial
  Scenario: one season pack is sent to the client once for two wanted episodes (SKADI-T-0402)
    Given a run "ep1" for "S03E01" that chose "House.of.the.Dragon.S03.1080p.AMZN.WEB-DL.x265-GRP"
    And a run "ep2" for "S03E02" that chose "House.of.the.Dragon.S03.1080p.AMZN.WEB-DL.x265-GRP"
    When run "ep1" snatches
    And run "ep2" snatches
    Then the client holds 1 transfer

  @C12 @passing @serial
  Scenario: two sweeps racing on one acquirable start one run
    Given a run "ep1" for "S03E01" that chose "House.of.the.Dragon.S03.1080p.AMZN.WEB-DL.x265-GRP"
    And run "ep1" was launched by the sweep
    Then the sweep cannot start another run for "S03E01"

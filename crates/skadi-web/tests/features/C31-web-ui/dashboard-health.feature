Feature: C31 overview dashboard and health badges
  The Overview renders /health/checks as badges and the library counts per
  enabled domain (Radarr's System → Health + the home cards).

  @C31 @passing
  Scenario: health badges render one row per check with a status dot
    Given the harness daemon with the movies domain enabled
    When the operator opens "/"
    Then the heading "Overview" is visible
    And a health badge named "daemon" shows status "ok"
    And a health badge named "database" shows status "ok"

  @C31 @passing
  Scenario: the download worker and VPN panels degrade gracefully when absent
    Given the harness daemon with the movies domain enabled
    When the operator opens "/downloads"
    Then the page shows text "worker" with a "down" or "not running" state
    And the page does not show an error dialog

  @C31 @gap
  Scenario: a library-root problem is visible on the Overview (SKADI-T-0430)
    Given the harness daemon with a library root that does not exist
    When the operator opens "/"
    Then a health badge whose name starts with "root" shows status "fail"

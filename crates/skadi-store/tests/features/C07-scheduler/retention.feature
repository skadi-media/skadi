Feature: Scheduler — retention purges the store supports (C07)
  Skadi has no task scheduler of its own: the hunter's sweep tick (every 300 s
  per domain) piggybacks the retention work — expired blocklist entries and
  history older than 90 days — and the download worker reclaims lapsed leases.
  Sonarr runs the equivalent housekeeping as named scheduled tasks.

  @C07 @passing
  Scenario: history older than the retention window is purged, newer rows stay
    Given a migrated sqlite store
    And 3 history rows aged 100 days and 2 rows aged 10 days
    When history older than 90 days is purged
    Then 3 rows were purged and 2 history rows remain

  @C07 @passing
  Scenario: history can also be capped to the newest rows
    Given a migrated sqlite store
    And 4 history rows aged 5 days and 3 rows aged 1 days
    When history is trimmed to the newest 3 rows
    Then 4 rows were purged and 3 history rows remain

  @C07 @passing
  Scenario: an expired blocklist entry stops vetoing while a permanent one stays
    Given a migrated sqlite store
    And a blocklist entry "magnet:old" that expired 5 minutes ago and a permanent entry "magnet:bad"
    When expired blocklist entries are purged
    Then "magnet:old" is no longer blocked and "magnet:bad" still is

  @C07 @passing
  Scenario: a download whose worker died is handed back to the queue
    Given a migrated sqlite store
    And a download claimed by worker "w1" whose 120-second lease has lapsed
    When expired download leases are reclaimed
    Then the download is queued again with no worker

  @C07 @gap
  Scenario: the hunter trace stream has a retention policy
    Given a migrated sqlite store
    And 5 trace events aged 200 days
    Then trace events older than 90 days can be purged

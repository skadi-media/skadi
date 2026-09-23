Feature: C29 calendar
  Sonarr/Radarr: `/calendar?start=&end=` lists upcoming/aired episodes and
  releases, and `/feed/v3/calendar/Sonarr.ics?apikey=` is a subscribable iCal
  feed.

  Skadi has one calendar across every enabled domain rather than one per app
  (SKADI-T-0465), assembled from the same LibraryProvider registry `/library`
  uses. Domains adopt it incrementally: a provider that has not implemented
  `calendar` contributes nothing rather than breaking the view.

  Background:
    Given a daemon running in open mode

  @C29 @passing @SKADI-T-0465
  Scenario: upcoming releases across domains are listed for a date window
    When the client requests GET "/api/v1/calendar?start=2026-09-01&end=2026-09-30"
    Then the response status is 200

  @C29 @passing @SKADI-T-0465
  Scenario: the calendar is subscribable as an iCal feed
    When the client requests GET "/api/v1/calendar.ics"
    Then the response status is 200
    And the response header "content-type" starts with "text/calendar"

  @C29 @passing @SKADI-T-0465
  Scenario: a malformed date is refused rather than silently ignored
    When the client requests GET "/api/v1/calendar?start=last-tuesday&end=2026-09-30"
    Then the response status is 400

  @C29 @passing @SKADI-T-0465
  Scenario: an unbounded window is refused — the calendar is a view, not an export
    When the client requests GET "/api/v1/calendar?start=1900-01-01&end=2100-01-01"
    Then the response status is 400

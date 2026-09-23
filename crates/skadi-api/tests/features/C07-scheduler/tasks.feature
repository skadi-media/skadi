Feature: Scheduler / tasks — cadences and manual runs (C07)
  Skadi has no scheduler component: cadences are per-worker tokio intervals
  (supervisor 5 s; per-domain wanted sweep 300 s + RSS pass 60 s; metadata
  refresh from config; definition sync daily) and retention piggybacks the
  sweep tick. Sonarr/Radarr list every task under System -> Tasks with its
  interval, last/next run and a manual "run now".

  Background:
    Given a daemon running in open mode

  @C07 @passing
  Scenario: the built-in cadences are what the operator guide says
    Then the supervisor ticks every 5 s, the RSS pass every 60 s and history is kept 90 days

  @C07 @passing @serial
  Scenario: "search all missing" pokes every running domain worker immediately
    Given a subscription to the hunter sweep trigger
    When the client requests POST "/api/v1/search-all"
    Then the response status is 202
    And the hunter sweep trigger has been poked

  @C07 @gap
  Scenario: the wanted-sweep interval is an operator setting like Sonarr's RSS Sync Interval
    Then the wanted-sweep interval is an operator setting

  @C07 @gap
  Scenario: scheduled tasks are listed with their interval and last/next run
    When the client requests GET "/api/v1/system/tasks"
    Then the response status is 200
    And the response body is a JSON array of length 6

  @C07 @gap
  Scenario: a scheduled task can be run on demand
    When the client requests POST "/api/v1/system/tasks/history-cleanup/run"
    Then the response status is 202

Feature: C34 health check model, cache, warnings and config sanity
  Sonarr/Radarr's System → Health lists each problem with a level
  (ok / warning / error), a message, a "how to fix it" link and when it was
  last checked. The checks run on a schedule and the page reads the stored
  results, so one provider that is down cannot make the page slow.

  Skadi today returns a flat `{ name, status: ok|fail, detail }` list
  (`diagnostics.rs`), probes every provider live on each request with a 30 s
  budget, has no warning producer on the server, and says nothing when there
  is nothing configured. These scenarios are the specs for COLLIERY-I-0294:

  - result shape ................ SKADI-T-0679
  - cache + forced run .......... SKADI-T-0680
  - warn + config-sanity checks . SKADI-T-0681
  - VPN egress .................. SKADI-T-0683

  Checks are found by `id` (or, for the old projection, `name`), in either a
  bare JSON array or an object with a `checks` array, so the route can keep or
  change its envelope. The check ids are part of the spec:
  `database`, `worker`, `root`, `root:<domain>`, `disk-space`, `indexers`,
  `download-clients`, `domain:<domain>`, `indexer:<name>` and `vpn`.

  "the health checks have run" sends `POST /health/checks/run`. Until that
  route exists (SKADI-T-0680) a 404 is accepted, because `GET` still probes
  live; so the model, warn and VPN scenarios do not wait on the cache task.

  Background:
    Given a daemon running in open mode

  # ---- the result model (SKADI-T-0679) --------------------------------------

  @C34 @gap
  Scenario: every check result carries a severity, a message, a remediation and checked_at
    When the health checks have run
    And the client requests GET "/api/v1/health/checks"
    Then the response status is 200
    And every health check has an id, a label, a severity, a message and a checked_at time
    And every health check severity is one of "ok,warn,error"
    And the health check "database" has severity "ok"

  @C34 @gap
  Scenario: a failing check says how to fix it
    When the health checks have run
    And the client requests GET "/api/v1/health/checks"
    Then the health check "worker" has severity "error"
    And the health check "worker" has a remediation
    And every health check with severity "error" has a remediation

  # ---- the cache and the forced run (SKADI-T-0680) --------------------------

  @C34 @gap
  Scenario: the health checks are served from the cache without probing a provider
    Given an indexer "countixr" whose server counts its requests
    When the client requests GET "/api/v1/health/checks"
    And the client requests GET "/api/v1/health/checks"
    Then the response status is 200
    And the server of indexer "countixr" has received 0 requests

  @C34 @gap
  Scenario: a provider that hangs does not slow the health checks request
    Given an indexer "hangixr" whose server accepts connections but never answers
    When the client requests GET "/api/v1/health/checks"
    Then the response status is 200
    And the request took less than 100 ms
    And the health check "indexer:hangixr" is pending

  @C34 @gap
  Scenario: a check that has never run reports as pending, and the supervisor tick fills it in
    When the client requests GET "/api/v1/health/checks"
    Then the health check "database" is pending
    When the supervisor refreshes the health checks
    And the client requests GET "/api/v1/health/checks"
    Then the health check "database" has severity "ok"
    And the health check "database" has a checked_at time

  @C34 @gap
  Scenario: POST /health/checks/run refreshes the cache and returns the new results
    When the client requests POST "/api/v1/health/checks/run"
    Then the response status is 200
    And the health check "database" has severity "ok"
    And the checked_at time of the health check "database" is remembered as "first"
    When 1100 ms pass
    And the client requests POST "/api/v1/health/checks/run"
    Then the response status is 200
    And the health check "database" was checked after "first"
    When the client requests GET "/api/v1/health/checks"
    Then the health check "database" was checked after "first"

  # ---- free space (SKADI-T-0681) --------------------------------------------

  @C34 @gap
  Scenario: a library root that is 80 % full is a warning
    Given the library root points at an existing writable directory
    And the library root's filesystem is 80 % full
    When the health checks have run
    And the client requests GET "/api/v1/health/checks"
    Then the health check "disk-space" has severity "warn"
    And the health check "disk-space" message contains "80"

  @C34 @gap
  Scenario: a library root that is 95 % full is an error
    Given the library root points at an existing writable directory
    And the library root's filesystem is 95 % full
    When the health checks have run
    And the client requests GET "/api/v1/health/checks"
    Then the health check "disk-space" has severity "error"
    And the health check "disk-space" has a remediation

  # ---- config sanity (SKADI-T-0681) -----------------------------------------

  @C34 @gap
  Scenario: no indexers configured is an error that names the settings page
    Given no indexer is configured
    When the health checks have run
    And the client requests GET "/api/v1/health/checks"
    Then the health check "indexers" has severity "error"
    And the health check "indexers" remediation contains "Indexers"

  @C34 @gap
  Scenario: no download client configured is an error that names the settings page
    Given no download client is configured
    When the health checks have run
    And the client requests GET "/api/v1/health/checks"
    Then the health check "download-clients" has severity "error"
    And the health check "download-clients" remediation contains "Downloaders"

  @C34 @gap
  Scenario: no library root configured is an error that names the setting
    Given the library root is not set
    When the health checks have run
    And the client requests GET "/api/v1/health/checks"
    Then the health check "root" has severity "error"
    And the health check "root" remediation contains "library.root"

  @C34 @gap
  Scenario: an enabled domain whose root folder is missing is an error
    Given the library root points at an existing writable directory
    And the "movies" domain is enabled
    When the health checks have run
    And the client requests GET "/api/v1/health/checks"
    Then the health check "root:movies" has severity "error"
    And the health check "root:movies" message contains "movie"
    And the health check "root:movies" has a remediation

  # ---- worker failures (SKADI-T-0681) ---------------------------------------

  @C34 @gap
  Scenario: a domain worker that keeps dying is an error check
    Given the "movies" domain is enabled
    And the supervisor has recorded 3 failures of the "movies" domain worker
    When the health checks have run
    And the client requests GET "/api/v1/health/checks"
    Then the health check "domain:movies" has severity "error"
    And the health check "domain:movies" message contains "3"
    And the health check "domain:movies" has a remediation

  @C34 @gap
  Scenario: a stale download-worker heartbeat is an error check
    Given the download worker "worker-a" last heartbeated 10 minutes ago
    When the health checks have run
    And the client requests GET "/api/v1/health/checks"
    Then the health check "worker" has severity "error"
    And the health check "worker" message contains "worker-a"
    And the health check "worker" has a remediation

  # ---- VPN egress (SKADI-T-0683) --------------------------------------------

  @C34 @gap @serial
  Scenario: the worker's egress differs from gluetun's exit, so the VPN check is an error
    Given a gluetun whose tunnel is "running" with the exit IP "203.0.113.7"
    And the download worker "worker-a" heartbeated just now with the egress IP "198.51.100.9"
    When the health checks have run
    And the client requests GET "/api/v1/health/checks"
    Then the health check "vpn" has severity "error"
    And the health check "vpn" message contains "198.51.100.9"
    And the health check "vpn" message contains "203.0.113.7"
    And the health check "vpn" has a remediation

  @C34 @gap @serial
  Scenario: the worker's egress equals gluetun's exit, so the VPN check is ok
    Given a gluetun whose tunnel is "running" with the exit IP "203.0.113.7"
    And the download worker "worker-a" heartbeated just now with the egress IP "203.0.113.7"
    When the health checks have run
    And the client requests GET "/api/v1/health/checks"
    Then the health check "vpn" has severity "ok"

  @C34 @gap @serial
  Scenario: a VPN tunnel that is down is an error
    Given a gluetun whose tunnel is "stopped" with the exit IP "203.0.113.7"
    When the health checks have run
    And the client requests GET "/api/v1/health/checks"
    Then the health check "vpn" has severity "error"
    And the health check "vpn" has a remediation

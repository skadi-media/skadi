Feature: C35 backup and restore
  Sonarr/Radarr: System → Backups lists scheduled + manual backups (config +
  database zip), `POST /system/backup` creates one, `POST /system/backup/restore/{id}`
  restores. Skadi has no backup surface at all (no route, no scheduler, no
  restore path); the operator's only option is a Postgres dump by hand.

  Background:
    Given a daemon running in open mode
    And backups are written to a temporary directory

  @C35 @passing @SKADI-T-0463
  Scenario: backups can be listed
    When the client requests GET "/api/v1/system/backup"
    Then the response status is 200

  @C35 @passing @SKADI-T-0463
  Scenario: a manual backup can be requested
    When the client requests POST "/api/v1/system/backup"
    Then the response status is 202

  @C35 @passing @SKADI-T-0463
  Scenario: a backup can be restored
    # Restoring "latest" needs one to exist; a bare restore against an empty
    # directory is a 404, which the next scenario pins.
    When the client requests POST "/api/v1/system/backup"
    Then the response status is 202
    When the client requests POST "/api/v1/system/backup/restore/latest"
    Then the response status is 202

  @C35 @passing @SKADI-T-0463
  Scenario: a backup captures the settings and restores them after they are deleted
    Given a stored indexers setting remembered as "ix" with body:
      """
      { "kind": "torznab", "name": "nzbgeek", "base_url": "http://127.0.0.1:1", "categories": [2000], "api_key": "abc" }
      """
    When the client requests POST "/api/v1/system/backup"
    Then the response status is 202
    When the client requests DELETE "/api/v1/settings/indexers/<ix>"
    Then the response status is 204
    When the client requests GET "/api/v1/settings/indexers"
    Then the response body is a JSON array of length 0
    When the client requests POST "/api/v1/system/backup/restore/latest"
    Then the response status is 202
    When the client requests GET "/api/v1/settings/indexers"
    Then the response body is a JSON array of length 1
    And the response field "0.body.name" is "nzbgeek"

  @C35 @passing @SKADI-T-0463
  Scenario: restoring a backup that does not exist is a 404, and a traversing id is refused
    When the client requests POST "/api/v1/system/backup/restore/latest"
    Then the response status is 404
    When the client requests POST "/api/v1/system/backup/restore/..%2F..%2Fetc%2Fpasswd"
    Then the response status is 400

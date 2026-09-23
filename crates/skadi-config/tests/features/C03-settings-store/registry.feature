Feature: Settings store — the config key registry and env seeding (C03)
  skadi-config is the single source of truth for which process-config keys
  exist, their type, default and tier. `SKADI_*` env vars are seeded into the
  `config` table at boot (Tier 1); Tier 0 keys are read directly from env.
  Sonarr/Radarr keep the same split (config.xml for bootstrap, DB for the rest).

  @C03 @passing
  Scenario Outline: a dotted config key maps to one SKADI_ environment variable and back
    Then the config key "<key>" is read from the environment variable "<env>"

    Examples:
      | key                   | env                       |
      | database_url          | SKADI_DATABASE_URL        |
      | secret_key            | SKADI_SECRET_KEY          |
      | bind_addr             | SKADI_BIND_ADDR           |
      | api_token             | SKADI_API_TOKEN           |
      | library.root          | SKADI_LIBRARY_ROOT        |
      | worker.download_dir   | SKADI_WORKER_DOWNLOAD_DIR |
      | cardigann_proxy_url   | SKADI_CARDIGANN_PROXY_URL |
      | naming.space          | SKADI_NAMING_SPACE        |

  @C03 @passing
  Scenario: every registered key round-trips and foreign variables are ignored
    Then every registered key round-trips through its environment variable name
    And the environment variable "SKADI_NOT_A_KEY" is not a config key
    And the environment variable "PATH" is not a config key

  @C03 @passing
  Scenario: only the database URL and the secret key are pre-table (Tier 0)
    Then the pre-table keys are exactly "database_url, secret_key"

  @C03 @passing
  Scenario Outline: registry defaults preserve the zero-config behaviour
    Then the key "<key>" defaults to "<default>"
    And the key "<key>" is declared as a <kind>

    Examples:
      | key                            | default                 | kind   |
      | database_url                   | sqlite://./skadi.db     | string |
      | bind_addr                      | 127.0.0.1:8080          | string |
      | mode                           | production              | string |
      | library.root                   | /data                   | path   |
      | min_seeders                    | 1                       | u16    |
      | sweep_max_concurrent           | 4                       | u16    |
      | metadata_refresh_interval_secs | 21600                   | u64    |
      | cardigann_sync_enabled         | true                    | bool   |
      | worker.up_limit_bps            | 500000                  | u64    |
      | worker.lease_secs              | 120                     | u64    |

  @C03 @passing
  Scenario: an unregistered key has no default
    Then the key "nope" is not registered

  @C03 @passing @serial
  Scenario: the boot seeder collects set, non-empty Tier-1 variables only
    Given the environment variable "SKADI_API_TOKEN" is "tok"
    And the environment variable "SKADI_MODE" is "testing"
    And the environment variable "SKADI_TMDB_API_KEY" is ""
    And the environment variable "SKADI_DATABASE_URL" is "postgres://x"
    And the environment variable "SKADI_SOMETHING_ELSE" is "nope"
    When the boot seeder collects the environment
    Then the seeder collected "api_token" as "tok"
    And the seeder collected "mode" as "testing"
    And the seeder did not collect "tmdb_api_key"
    And the seeder did not collect "database_url"
    And the seeder did not collect "secret_key"

  @C03 @passing @serial
  Scenario: an unset variable is not seeded, so a runtime value survives the next boot
    Given the environment variable "SKADI_NAMING_SPACE" is unset
    When the boot seeder collects the environment
    Then the seeder did not collect "naming.space"

  @C03 @bug @SKADI-T-0427
  Scenario: every config key the daemon writes from its HTTP layer is a registered key
    Then every config key the daemon's naming endpoint writes is registered

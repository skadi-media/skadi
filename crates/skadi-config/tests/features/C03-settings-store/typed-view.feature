Feature: Settings store — the typed read contract (C03)
  A `ConfigView` is a snapshot of the `config` table; every key resolves as
  table value -> registry default, parsed to the requested type. Bad values are
  typed errors, never silent fallbacks.

  @C03 @passing
  Scenario Outline: an empty table resolves every key to its registry default
    Given an empty config table
    When the <kind> value of "<key>" is read
    Then the value is "<value>"

    Examples:
      | kind     | key                 | value                   |
      | string   | bind_addr           | 127.0.0.1:8080          |
      | optional | api_token           | None                    |
      | u16      | worker.port_hi      | 16891                   |
      | u64      | worker.poll_secs    | 3                       |
      | path     | worker.download_dir | /data/downloads/complete|
      | path     | worker.watch_dir    | none                    |
      | bool     | worker.migrate      | false                   |
      | mode     | mode                | Production              |

  @C03 @passing
  Scenario: a stored value overrides the default and parses to its declared type
    Given a config table holding:
      | key            | value        |
      | bind_addr      | 0.0.0.0:9000 |
      | worker.port_lo | 20000        |
      | worker.migrate | yes          |
      | mode           | just-go      |
    When the string value of "bind_addr" is read
    Then the value is "0.0.0.0:9000"
    When the u16 value of "worker.port_lo" is read
    Then the value is "20000"
    When the bool value of "worker.migrate" is read
    Then the value is "true"
    When the mode value of "mode" is read
    Then the value is "JustGo"

  @C03 @passing
  Scenario: an empty stored string reads as "unset" for optional and path keys
    Given a config table holding:
      | key              | value |
      | api_token        |       |
      | worker.watch_dir |       |
    When the optional value of "api_token" is read
    Then the value is "None"
    When the path value of "worker.watch_dir" is read
    Then the value is "none"

  @C03 @passing
  Scenario: an unregistered key is a typed error on read
    Given an empty config table
    When the string value of "not.a.key" is read
    Then the read fails with an unknown-key error for "not.a.key"

  @C03 @passing
  Scenario Outline: a malformed stored value is a parse error naming the key
    Given a config table holding:
      | key   | value   |
      | <key> | <value> |
    When the <kind> value of "<key>" is read
    Then the read fails with a parse error naming "<key>"

    Examples:
      | kind | key                 | value  |
      | u16  | worker.port_lo      | x      |
      | u16  | worker.port_lo      | 70000  |
      | u64  | worker.poll_secs    | -1     |
      | bool | cardigann_sync_enabled | maybe |

  @C03 @passing
  Scenario Outline: the operational mode parses tolerantly
    Then the mode string "<input>" parses as <mode>

    Examples:
      | input     | mode       |
      | production| Production |
      | PROD      | Production |
      | just-go   | JustGo     |
      | Just_Go   | JustGo     |
      | testing   | Testing    |
      |  Test     | Testing    |

  @C03 @passing
  Scenario: an unknown mode is rejected rather than defaulted
    Then the mode string "staging" is rejected

  @C03 @passing @SKADI-T-0524
  Scenario: a value is validated against its declared type when it is saved, not when it is next read
    Then the schema rejects "x" as a value for "worker.port_lo" before it is stored

  @C03 @passing @SKADI-T-0524
  Scenario: related keys are validated against each other on save
    Then the schema rejects a worker port range whose low end is above its high end

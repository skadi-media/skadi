Feature: Workflow engine — Cloacina's isolated storage target (C08)
  The hunter runs its acquire workflows on a Cloacina runner whose state is
  kept apart from skadi's own tables: a `cloacina` database + `cloacina_hunter`
  schema on Postgres, a sibling `hunter.db` file on SQLite. Bootstrap creates
  the Postgres database on demand; SQLite needs nothing.

  Background:
    Given a daemon running in open mode

  @C08 @passing
  Scenario Outline: the engine's storage target is derived from the skadi database URL
    Then the workflow engine for "<skadi>" is isolated at "<target>" with schema "<schema>"

    Examples:
      | skadi                                          | target                                             | schema          |
      | postgres://user:pw@localhost/skadi             | postgres://user:pw@localhost/cloacina              | cloacina_hunter |
      | postgresql://u:p@db:5432/skadi?sslmode=require | postgresql://u:p@db:5432/cloacina?sslmode=require  | cloacina_hunter |
      | sqlite://./data/skadi.db                       | sqlite://./data/hunter.db                          | none            |
      | sqlite:///var/lib/skadi/skadi.db               | sqlite:///var/lib/skadi/hunter.db                  | none            |

  @C08 @passing
  Scenario: an unsupported database scheme is refused before any runner is built
    Then deriving a workflow target for "mysql://nope" is a configuration error
    And ensuring the cloacina database for a sqlite URL is a no-op

  @C08 @passing
  Scenario: a runner builds against the scenario database, migrates its own schema and shuts down cleanly
    When a workflow runner is built against the scenario database and shut down
    Then the hunter's own database sits beside the store

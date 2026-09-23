Feature: Edition-kind registry (C28) — Radarr editions as a DB-backed, editable vocabulary

  Background:
    Given an empty movies library

  @C28 @passing
  Scenario: a fresh database is seeded with the six builtin kinds in name order
    When the edition kinds are listed
    Then there are 6 builtin kinds
    And the kinds include "Theatrical, Extended, Director's Cut, Ultimate Cut, IMAX, Remastered"
    And the kinds are ordered by name

  @C28 @passing
  Scenario: builtin kinds cannot be deleted
    When the builtin kind "Theatrical" is deleted
    Then the delete is rejected as a validation error
    And the kind "Theatrical" still exists

  @C28 @passing
  Scenario: a custom kind can be created, found by tag, round-tripped and deleted
    When a custom kind "Final Cut" tagged "Final Cut" matching "final cut" is created
    Then the kind "Final Cut" can be found by tag "Final Cut" and is not builtin
    And the kind "Final Cut" round-trips through JSON
    When the custom kind "Final Cut" is deleted
    Then the kind "Final Cut" is gone

  @C28 @passing @SKADI-T-0452
  Scenario: a normalized tag that collides with an existing kind is rejected
    When a custom kind "Extended Again" tagged "Extended" matching "extended again" is created
    Then the second kind write is rejected as a validation error

  @C28 @passing @SKADI-T-0452
  Scenario: a kind with an empty name is rejected
    When a kind with an empty name tagged "Nameless" is created
    Then the kind write is rejected as a validation error

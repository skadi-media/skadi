Feature: Acquirable-unit kinds for television (C28)
  The C28 registry is movies-only today; television has no season/episode-type registry.

  @C28 @gap
  Scenario: television exposes an acquirable-unit kind registry (episode types such as regular / special / multi-part)
    Given an empty television library
    Then the television domain has an acquirable-unit kind registry

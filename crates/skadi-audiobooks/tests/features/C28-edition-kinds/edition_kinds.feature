Feature: Acquirable-unit kinds for audiobooks (C28)
  Readarr models narration/abridgement editions under one book; Skadi has no audiobook edition registry.

  @C28 @gap
  Scenario: Full Cast / Booktrack / Dramatized editions are kinds of one book, not separate monitored books (SKADI-T-0400)
    Given an empty audiobook library
    Then audiobook editions such as Full Cast or Booktrack are modelled as acquirable-unit kinds

Feature: Token substitution and empty-group tidy (C21)
  The engine's `{Token}` grammar: single-pass substitution, unknown tokens render
  empty, values are never re-scanned, and empty `()`/`[]` groups left by missing
  tokens collapse. Mirrors the Sonarr/Radarr naming token grammar.

  @C21 @passing
  Scenario: known tokens substitute and unknown tokens render empty
    Given the token "Title" is "The Matrix"
    And the token "Year" is "1999"
    When the template "{Title} ({Year}) {Nope}" is rendered
    Then the result is "The Matrix (1999) "

  @C21 @passing
  Scenario: a token value containing braces is inserted verbatim and not re-scanned
    Given the token "TmdbTag" is "{tmdb-603}"
    And the token "tmdb-603" is "INJECTED"
    When the template "{TmdbTag}" is rendered
    Then the result is "{tmdb-603}"

  @C21 @passing
  Scenario: an unmatched opening brace is emitted literally
    Given no tokens
    When the template "a {oops" is rendered
    Then the result is "a {oops"

  @C21 @passing
  Scenario: empty bracket groups left by missing tokens are tidied away
    When "The Matrix () {tmdb-603}" is tidied
    Then the result is "The Matrix  {tmdb-603}"
    When "[()]" is tidied
    Then the result is ""
    When "keep (1999)" is tidied
    Then the result is "keep (1999)"

  @C21 @passing
  Scenario: a component renders end to end: substitute, tidy, sanitize
    Given the token "Title" is "What: If?"
    And the token "Year" is empty
    When the component template "{Title} ({Year})" is rendered
    Then the result is "What_If"

  @C21 @passing
  Scenario: self-punctuating optional parts leave no dangling separator
    Given the episode token set the domain supplies
    And the token "EpisodeTitlePart" is empty
    When the component template "{SeriesTitleKebab} - {Episode}{EpisodeTitlePart}" is rendered
    Then the result is "the-wire_-_S03E05"

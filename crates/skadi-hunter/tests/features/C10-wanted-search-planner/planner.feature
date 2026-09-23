Feature: Wanted / search planner — the seed the hunter searches with
  The domain's `WantedQuery` hands the hunter an `AcquireSeed`; the hunter
  turns its `SearchSpec` into the indexer query (REQ-WANTED.5/.6), matches
  RSS feed entries against wanted titles (REQ-WANTED.15) and re-checks a
  not-found item on an escalating backoff (REQ-ORCH.17).

  @C10 @passing
  Scenario: an episode search carries season and episode to the indexer
    Given a search spec for episode "Lioness" S3E2
    Then the indexer query carries season 3 and episode 2
    And the indexer query is scoped to category 5000
    And the search spec round-trips through the workflow context

  @C10 @passing
  Scenario: a season-pack search carries the season only
    Given a search spec for season pack "Lioness" S3
    Then the indexer query carries season 3 and no episode

  @C10 @passing
  Scenario: a movie search carries the year and external ids
    Given a search spec for movie "Heat" (1995) with tmdb id 949
    Then the indexer query carries year 1995 and tmdb id 949
    And the indexer query is scoped to category 2000

  @C10 @passing
  Scenario: RSS feed entries match wanted items by their parsed title
    Then the RSS feed entry "Sintel 2010 1080p BluRay x264-GRP" matches the wanted title "Sintel"
    And the RSS feed entry "The.Matrix.1999.1080p.BluRay.x264-GRP" does not match the wanted title "Matrix Reloaded"

  @C10 @passing @serial
  Scenario: a first miss is re-checked in six hours
    Given a registered movie domain with an in-memory status sink
    And a first-acquisition run "run-a" for "ed-1" searching "Nothing Here"
    When run "run-a" searches and decides
    Then the status of "ed-1" is Failed not-found with 1 attempt and a re-check in about 6 hours
    And "ed-1" is not left in flight

  @C10 @passing @serial
  Scenario: repeated misses back off to twelve hours, a day, then three days
    Given a registered movie domain with an in-memory status sink
    And a first-acquisition run "run-a" for "ed-1" searching "Nothing Here"
    And "ed-1" already failed not-found 1 time
    When run "run-a" searches and decides
    Then the status of "ed-1" is Failed not-found with 2 attempts and a re-check in about 12 hours
    Given a first-acquisition run "run-b" for "ed-2" searching "Nothing Here"
    And "ed-2" already failed not-found 2 times
    When run "run-b" searches and decides
    Then the status of "ed-2" is Failed not-found with 3 attempts and a re-check in about 24 hours
    Given a first-acquisition run "run-c" for "ed-3" searching "Nothing Here"
    And "ed-3" already failed not-found 7 times
    When run "run-c" searches and decides
    Then the status of "ed-3" is Failed not-found with 8 attempts and a re-check in about 72 hours

  @C10 @passing @serial
  Scenario: an upgrade run that finds nothing leaves the imported file alone
    Given a registered movie domain with an in-memory status sink
    And a first-acquisition run "run-a" for "ed-1" searching "Nothing Here"
    And run "run-a" is an upgrade run
    And "ed-1" is already Imported
    When run "run-a" searches and decides
    Then the status of "ed-1" is Imported

  @C10 @passing @SKADI-T-0431
  Scenario: the RSS fast pass matches an episode feed entry to its show
    Then the RSS feed entry "Lioness.S03E02.1080p.WEB-DL.h264-GRP" matches the wanted title "Lioness"

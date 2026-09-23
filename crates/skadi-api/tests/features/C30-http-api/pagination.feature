Feature: C30 list pagination
  Radarr/Sonarr v3 paged resources answer `{ page, pageSize, sortKey,
  totalRecords, records }`. Skadi uses `?limit=&offset=`; only /history reports
  a total (X-Total-Count header).

  Background:
    Given a daemon running in open mode

  @C30 @passing
  Scenario: history defaults to 50 newest rows and reports the total in a header
    Given 60 history rows of which 5 are failed imports
    When the client requests GET "/api/v1/history"
    Then the response status is 200
    And the response body is a JSON array of length 50
    And the response header "x-total-count" is "60"

  @C30 @passing
  Scenario: history pages with limit and offset, the total ignores paging
    Given 60 history rows of which 5 are failed imports
    When the client requests GET "/api/v1/history?limit=10&offset=55"
    Then the response body is a JSON array of length 5
    And the response header "x-total-count" is "60"

  @C30 @passing
  Scenario: history filters by event and reason code
    Given 60 history rows of which 5 are failed imports
    When the client requests GET "/api/v1/history?event=failed"
    Then the response body is a JSON array of length 5
    And the response header "x-total-count" is "5"
    When the client requests GET "/api/v1/history?reason_code=import_failed&limit=2"
    Then the response body is a JSON array of length 2
    And the response header "x-total-count" is "5"

  @C30 @passing
  Scenario: a garbage time bound on history is a validation error
    When the client requests GET "/api/v1/history?since=yesterday"
    Then the response status is 400
    And the response is a JSON error of kind "validation"

  @C30 @passing
  Scenario: history counts are whole-table totals per event
    Given 60 history rows of which 5 are failed imports
    When the client requests GET "/api/v1/history/counts"
    Then the response field "total" is "60"
    And the response field "failed" is "5"
    And the response field "grabbed" is "55"

  @C30 @passing
  Scenario: one history event is readable by id and an unknown id is 404
    Given 60 history rows of which 5 are failed imports
    When the client requests GET "/api/v1/history/h-0003"
    Then the response status is 200
    And the response field "event" is "failed"
    And the response field "reason_code" is "import_failed"
    When the client requests GET "/api/v1/history/h-9999"
    Then the response status is 404
    And the response is a JSON error of kind "not_found"

  @C30 @passing
  Scenario: traces list newest first with a default page and scope to one item
    Given 3 trace events for "movie:a"
    And 2 trace events for "movie:b"
    When the client requests GET "/api/v1/traces"
    Then the response body is a JSON array of length 5
    When the client requests GET "/api/v1/traces?acquirable=movie:b"
    Then the response body is a JSON array of length 2
    And the response field "0.acquirable_ref" is "movie:b"
    When the client requests GET "/api/v1/traces?limit=2"
    Then the response body is a JSON array of length 2

  @C30 @passing
  Scenario: decisions are paged and carry the release key and explanation
    Given a recorded decision for "movie:a" choosing "Movie.A.1080p" with release key "k-a"
    And a recorded decision for "movie:b" choosing "Movie.B.1080p" with release key "k-b"
    When the client requests GET "/api/v1/decisions?limit=1"
    Then the response body is a JSON array of length 1
    When the client requests GET "/api/v1/decisions?acquirable=movie:a"
    Then the response body is a JSON array of length 1
    And the response field "0.release_key" is "k-a"
    And the response field "0.explanation.reason" is "bdd"

  @C30 @passing
  Scenario: the unified library pages across domains after aggregation
    Given the daemon compiles in the "movies" domain
    And the "movies" domain is enabled
    And the "movies" library holds:
      | title       | monitored | status   |
      | Alpha       | yes       | missing  |
      | Beta        | yes       | imported |
      | Gamma       | no        | missing  |
    When the client requests GET "/api/v1/library"
    Then the response body is a JSON array of length 3
    When the client requests GET "/api/v1/library?limit=2&offset=2"
    Then the response body is a JSON array of length 1
    And the response field "0.title" is "Gamma"
    When the client requests GET "/api/v1/library?monitored=true&q=alp"
    Then the response body is a JSON array of length 1
    And the response field "0.title" is "Alpha"

  @C30 @passing
  Scenario: a disabled domain contributes nothing to the library
    Given the daemon compiles in the "movies" domain
    And the "movies" library holds:
      | title | monitored | status  |
      | Alpha | yes       | missing |
    When the client requests GET "/api/v1/library"
    Then the response body is a JSON array of length 0

  @C30 @passing
  Scenario: the wanted view keeps only monitored items with unsatisfied editions and summarises why
    Given the daemon compiles in the "movies" domain
    And the "movies" domain is enabled
    And the "movies" library holds:
      | title | monitored | status   |
      | Alpha | yes       | missing  |
      | Beta  | yes       | imported |
      | Gamma | no        | missing  |
      | Delta | yes       | failed   |
    When the client requests GET "/api/v1/wanted"
    Then the response field "summary.items" is "2"
    And the response field "summary.editions" is "2"
    And the response field "summary.by_status.missing" is "1"
    And the response field "summary.by_status.failed" is "1"
    When the client requests GET "/api/v1/wanted?limit=1&offset=1"
    Then the response field "items.0.title" is "Delta"
    And the response field "summary.items" is "2"

  @C30 @passing @SKADI-T-0468
  Scenario: library pages report the total so a client can render page controls
    Given the daemon compiles in the "movies" domain
    And the "movies" domain is enabled
    And the "movies" library holds:
      | title | monitored | status  |
      | Alpha | yes       | missing |
      | Beta  | yes       | missing |
    When the client requests GET "/api/v1/library?limit=1"
    Then the response body is a JSON array of length 1
    And the response header "x-total-count" is "2"

  @C30 @passing @SKADI-T-0468
  Scenario: every paged list endpoint reports its total
    Given 60 history rows of which 5 are failed imports
    And 3 trace events for "movie:a"
    And a recorded decision for "movie:a" choosing "Movie.A.1080p" with release key "k-a"
    When the client requests GET "/api/v1/history?limit=1"
    Then the response header "x-total-count" is "60"
    When the client requests GET "/api/v1/traces?limit=1"
    Then the response header "x-total-count" is "3"
    When the client requests GET "/api/v1/decisions?limit=1"
    Then the response header "x-total-count" is "1"

  @C30 @gap
  Scenario: Sonarr-style page envelope with sort keys on history
    Given 60 history rows of which 5 are failed imports
    When the client requests GET "/api/v1/history?page=2&pageSize=10&sortKey=date&sortDirection=descending"
    Then the response field "page" is "2"
    And the response field "pageSize" is "10"
    And the response field "totalRecords" is "60"
    And the response field "records" is present

  # --- SKADI-T-0495 (P9 performance pass), measured against a prod-sized dump:
  # 1,818 movies / 24,013 episodes / 790 books / 1,336 downloads / 670 blocklist.
  # `/series` returned 16.5 MB in 430 ms; `/movies` 2.8 MB; `/books` 2.0 MB.

  @C30 @gap @SKADI-T-0494
  Scenario: the series list pages instead of returning every episode of every show
    Given the daemon compiles in the "television" domain
    And the "television" domain is enabled
    When the client requests GET "/api/v1/series?limit=10"
    Then the response body is a JSON array of length 10
    And the response header "x-total-count" is present

  @C30 @gap @SKADI-T-0494
  Scenario Outline: every collection endpoint honours limit
    # Today these return the whole table: the response with ?limit=10 is
    # byte-for-byte identical to the response without it.
    When the client requests GET "<endpoint>?limit=10"
    Then the response is smaller than the same request without a limit

    Examples:
      | endpoint            |
      | /api/v1/movies      |
      | /api/v1/books       |
      | /api/v1/downloads   |
      | /api/v1/blocklist   |

  @C30 @gap @SKADI-T-0495
  Scenario: a paged library request does not scan the whole library
    # The sharper half of the finding: /library already accepts limit/offset, but
    # applies them AFTER awaiting every provider's items() and concatenating them
    # (library.rs:186-191, "cross-domain pagination applied after aggregation").
    # So paging shrinks the payload and not the work — 427 ms unpaginated,
    # 435 ms at limit=50 — and each page costs a full scan.
    Given the daemon compiles in the "movies" domain
    And the "movies" domain is enabled
    And the "movies" library holds 5000 items
    When the client requests GET "/api/v1/library?limit=50"
    Then the provider was asked for at most 50 items

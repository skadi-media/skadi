Feature: Cardigann engine — the filter pipeline and the Go-style template subset
  filters.rs / template.rs: what real definitions rely on to turn scraped cells
  into sizes, dates and hashes, and to build request URLs.

  @C15 @passing
  Scenario Outline: field filters transform scraped values as Jackett's do
    Then the filter "<filter>" with args <args> maps "<input>" to "<output>"

    Examples:
      | filter     | args                          | input                  | output               |
      | replace    | [".", " "]                    | The.Matrix.1999        | The Matrix 1999      |
      | re_replace | ['\\s+', ' ']                 | a   b    c             | a b c                |
      | regexp     | "([A-Fa-f0-9]{40})"           | hash: 0123456789abcdef0123456789abcdef01234567 ok | 0123456789abcdef0123456789abcdef01234567 |
      | split      | ["/", 1]                      | Movies/HD              | HD                   |
      | trim       | null                          |   padded               | padded               |
      | prepend    | "https://x.test"              | /dl/1.torrent          | https://x.test/dl/1.torrent |
      | append     | ".torrent"                    | /dl/1                  | /dl/1.torrent        |
      | tolower    | null                          | ABC                    | abc                  |
      | toupper    | null                          | abc                    | ABC                  |
      | urldecode  | null                          | a%20b                  | a b                  |
      | htmldecode | null                          | a &amp; b              | a & b                |
      | querystring| "id"                          | /details.php?id=42&x=1 | 42                   |

  @C15 @passing
  Scenario: relative-date filters resolve against the engine's clock
    Then the filter "timeago" with args null maps "2 hours ago" to "2026-09-06T10:00:00+00:00"
    And the filter "fuzzytime" with args null maps "Today" to "2026-09-06T12:00:00+00:00"

  @C15 @passing
  Scenario: templates substitute keywords, config and joined categories
    Given the search keywords "matrix"
    And the search categories "201,202"
    And the config override "apiurl" is "apibay.org"
    Then the template "https://{{ .Config.apiurl }}/q.php?q={{ .Keywords }}&cat={{ join .Categories "," }}" renders "https://apibay.org/q.php?q=matrix&cat=201,202"

  @C15 @passing
  Scenario: if/else branches on a keyword and on checkbox config
    Given the search keywords ""
    And the config override "disablesort" is "false"
    Then the template "{{ if .Keywords }}search/{{ .Keywords }}{{ else }}cat/Movies{{ end }}/1/" renders "cat/Movies/1/"
    And the template "{{ if and (.Keywords) (eq .Config.disablesort .False) }}sort-{{ else }}{{ end }}go" renders "go"
    Given the search keywords "heat"
    Then the template "{{ if .Keywords }}search/{{ .Keywords }}{{ else }}cat/Movies{{ end }}/1/" renders "search/heat/1/"

  @C15 @passing
  Scenario: .Query variables render the id params the adapter passes
    Given the search query param "imdbid" is "tt0133093"
    Then the template "{{ if .Query.imdbid }}imdb/{{ .Query.imdbid }}{{ else }}none{{ end }}" renders "imdb/tt0133093"

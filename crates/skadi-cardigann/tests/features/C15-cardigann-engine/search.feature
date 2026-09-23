Feature: Cardigann engine — running a definition's search against a tracker
  crates/skadi-cardigann/src/engine.rs executes Jackett/Prowlarr YAML tracker
  definitions: keyword filtering + URL encoding, `search.paths` / `search.inputs`
  rendering, JSON and HTML row extraction, field chains, category mapping and
  de-duplication. The tracker is a scripted in-process fetcher.

  @C15 @passing
  Scenario: a JSON tracker's rows map onto the release shape and keywords are URL-encoded
    Given the definition:
      """
      id: synthjson
      name: SynthJSON
      type: public
      caps:
        categorymappings: [{id: "1", cat: Movies}, {id: "2", cat: TV}]
        modes: {search: [q]}
      search:
        paths:
          - path: "search?q={{ .Keywords }}"
            response: {type: json}
        rows: {selector: "$"}
        fields:
          title: {selector: name}
          infohash: {selector: info_hash}
          size: {selector: size}
          seeders: {selector: seeders}
          leechers: {selector: leechers}
          category: {selector: category}
          date: {selector: added}
          imdb: {selector: imdb, optional: true}
      """
    And the site base URL "https://synth.test"
    And the search keywords "The Matrix"
    And the tracker answers URLs containing "search?q=" with HTTP 200 and body:
      """
      [{"name":"The Matrix 1999 1080p","info_hash":"AAAABBBB","size":"1.5 GB","seeders":"42","leechers":"3","category":"1","added":"1700000000","imdb":"tt0133093"},
       {"name":"The Matrix 1999 1080p","info_hash":"AAAABBBB","size":"1.5 GB","seeders":"42","leechers":"3","category":"1","added":"1700000000"},
       {"name":"Matrix Recap S01E01","info_hash":"CCCCDDDD","size":"700 MiB","seeders":"5","leechers":"1","category":"2","added":"1700000000"}]
      """
    When the engine runs the search
    Then 2 releases are extracted
    And the tracker was asked for a URL containing "https://synth.test/search?q=The%20Matrix"
    And release 1 has title "The Matrix 1999 1080p"
    And release 1 has info hash "AAAABBBB"
    And release 1 has size 1610612736
    And release 1 has 42 seeders and 3 leechers
    And release 1 has categories "Movies"
    And release 1 has date "2023-11-14T22:13:20+00:00"
    And release 1 has imdb "tt0133093"
    And release 2 has size 734003200
    And release 2 has categories "TV"

  @C15 @passing
  Scenario: an HTML tracker's rows are extracted with CSS selectors, attributes, a .Result chain and filters
    Given the definition:
      """
      id: tinytracker
      name: Tiny
      type: public
      caps:
        categorymappings: [{id: 1, cat: Movies}]
        modes: {search: [q]}
      search:
        paths:
          - path: "https://tiny.test/search?q={{ .Keywords }}"
        rows: {selector: "tr.r"}
        fields:
          title_raw: {selector: a.t}
          title:
            text: "{{ .Result.title_raw }}"
            filters:
              - name: replace
                args: [".", " "]
          download: {selector: a.dl, attribute: href}
          category: {text: "1"}
      """
    And the search keywords "matrix"
    And the tracker answers URLs containing "tiny.test/search?q=matrix" with HTTP 200 and body:
      """
      <html><body><table>
        <tr class="r"><td><a class="t">The.Matrix.1999</a><a class="dl" href="/get/1.torrent">dl</a></td></tr>
        <tr class="r"><td><a class="t">Dune.2021</a><a class="dl" href="/get/2.torrent">dl</a></td></tr>
      </table></body></html>
      """
    When the engine runs the search
    Then 2 releases are extracted
    And release 1 has title "The Matrix 1999"
    And release 1 has download "/get/1.torrent"
    And release 1 has categories "Movies"
    And release 2 has title "Dune 2021"

  @C15 @passing
  Scenario: search.inputs are rendered into the query string, and the site link resolves relative paths
    Given the definition:
      """
      id: inputtracker
      name: Input
      type: public
      caps:
        categorymappings: [{id: "1", cat: Audio/Audiobook}]
        modes: {search: [q]}
      search:
        paths:
          - path: "/"
            response: {type: json}
        inputs:
          s: "{{ .Keywords }}"
          cat: "{{ join .Categories \",\" }}"
        rows: {selector: "$"}
        fields:
          title: {selector: name}
          download: {selector: link}
      """
    And the site base URL "https://abb.test"
    And the search keywords "matrix"
    And the search categories "1"
    And the tracker answers URLs containing "/?s=matrix&cat=1" with HTTP 200 and body:
      """
      [{"name":"Some Audiobook 64kbps","link":"magnet:?xt=urn:btih:DEAD"}]
      """
    When the engine runs the search
    Then 1 release is extracted
    And release 1 has download "magnet:?xt=urn:btih:DEAD"

  @C15 @passing
  Scenario: a POST definition sends the inputs as the form body with the definition's headers
    Given the definition:
      """
      id: posttracker
      name: Post
      type: public
      caps:
        categorymappings: [{id: "1", cat: Movies}]
        modes: {search: [q]}
      search:
        paths:
          - path: "api/search"
            method: post
            response: {type: json}
        headers:
          X-Requested-With: ["XMLHttpRequest"]
        inputs:
          q: "{{ .Keywords }}"
        rows: {selector: "$"}
        fields:
          title: {selector: name}
          infohash: {selector: hash}
      """
    And the site base URL "https://post.test/"
    And the search keywords "heat"
    And the tracker answers URLs containing "api/search" with HTTP 200 and body:
      """
      [{"name":"Heat 1995","hash":"F00D"}]
      """
    When the engine runs the search
    Then 1 release is extracted
    And the tracker received a POST whose body contains "q=heat"
    And the tracker received a request with header "X-Requested-With" = "XMLHttpRequest"

  @C15 @passing
  Scenario: a multi-path definition keeps going when one path errors, and de-duplicates across paths
    Given the definition:
      """
      id: multipath
      name: Multi
      type: public
      caps:
        categorymappings: [{id: "1", cat: Movies}]
        modes: {search: [q]}
      search:
        paths:
          - path: "a?q={{ .Keywords }}"
            response: {type: json}
          - path: "b?q={{ .Keywords }}"
            response: {type: json}
          - path: "c?q={{ .Keywords }}"
            response: {type: json}
        rows: {selector: "$"}
        fields:
          title: {selector: name}
          infohash: {selector: hash}
      """
    And the site base URL "https://multi.test"
    And the search keywords "x"
    And the tracker answers URLs containing "/a?q=" with HTTP 500
    And the tracker answers URLs containing "/b?q=" with HTTP 200 and body:
      """
      [{"name":"Same 2020","hash":"AAAA"}]
      """
    And the tracker answers URLs containing "/c?q=" with HTTP 200 and body:
      """
      [{"name":"Same 2020","hash":"AAAA"},{"name":"Other 2021","hash":"BBBB"}]
      """
    When the engine runs the search
    Then 2 releases are extracted
    And the tracker received 3 requests

  @C15 @passing
  Scenario: keywordsfilters run before the keyword is templated into the path
    Given the definition:
      """
      id: kwf
      name: KWF
      type: public
      caps:
        categorymappings: [{id: "1", cat: Movies}]
        modes: {search: [q]}
      search:
        keywordsfilters:
          - name: re_replace
            args: ["[^a-zA-Z0-9]+", " "]
          - name: trim
        paths:
          - path: "q/{{ .Keywords }}"
            response: {type: json}
        rows: {selector: "$"}
        fields:
          title: {selector: name}
          infohash: {selector: hash}
      """
    And the site base URL "https://kwf.test"
    And the search keywords "The.Matrix: Reloaded!"
    And the tracker answers URLs containing "q/The%20Matrix%20Reloaded" with HTTP 200 and body:
      """
      [{"name":"The Matrix Reloaded 2003","hash":"AAAA"}]
      """
    When the engine runs the search
    Then 1 release is extracted

  @C15 @passing @SKADI-T-0498
  Scenario: a definition's .Query.IMDBID / .Query.Season / .Query.Ep render the id params the adapter passes as imdbid / season / ep
    Given the definition:
      """
      id: idsearch
      name: IdSearch
      type: public
      caps:
        categorymappings: [{id: "1", cat: Movies}]
        modes: {search: [q], movie-search: [q, imdbid], tv-search: [q, season, ep]}
      search:
        paths:
          - path: "search?imdb={{ .Query.IMDBID }}&s={{ .Query.Season }}&e={{ .Query.Ep }}&q={{ .Keywords }}"
            response: {type: json}
        rows: {selector: "$"}
        fields:
          title: {selector: name}
          infohash: {selector: hash}
      """
    And the site base URL "https://ids.test"
    And the search keywords "x"
    And the search query param "imdbid" is "tt0133093"
    And the search query param "season" is "3"
    And the search query param "ep" is "2"
    And the tracker answers URLs containing "search?" with HTTP 200 and body:
      """
      []
      """
    When the engine runs the search
    Then the tracker was asked for a URL containing "imdb=tt0133093&s=3&e=2"

  @C15 @passing @SKADI-T-0496
  Scenario: a search whose every path was refused by the tracker is a failure, not an empty healthy result
    Given the definition:
      """
      id: refused
      name: Refused
      type: public
      caps:
        categorymappings: [{id: "1", cat: Movies}]
        modes: {search: [q]}
      search:
        paths:
          - path: "search?q={{ .Keywords }}"
            response: {type: json}
        rows: {selector: "$"}
        fields:
          title: {selector: name}
      """
    And the site base URL "https://refused.test"
    And the search keywords "x"
    And the tracker answers URLs containing "search?q=" with HTTP 503 and body:
      """
      <title>Just a moment...</title><div id="challenge-platform"></div>
      """
    When the engine runs the search
    Then the search fails

  @C15 @passing
  Scenario: the real thepiratebay definition parses a recorded apibay response
    Given the real "thepiratebay" definition from the fixture library
    And the site base URL "https://thepiratebay.org"
    And the search keywords "sintel"
    And the tracker answers URLs containing "q.php" with the recorded response "thepiratebay-search.json"
    When the engine runs the search
    Then the tracker was asked for a URL containing "apibay.org/q.php?q=sintel"
    And release 1 has an info hash and a size

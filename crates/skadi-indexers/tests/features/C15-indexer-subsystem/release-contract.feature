Feature: The Release contract — blocklist identity, manual links and title normalisation
  What the rest of the pipeline may rely on from an indexer result
  (crates/skadi-indexers/src/lib.rs, normalize.rs).

  @C15 @passing
  Scenario: a magnet release is identified by its lowercased info hash regardless of trackers or display name
    Given a release "Movie" of size 1 fetched by magnet "magnet:?xt=urn:btih:DD8255ECDC7CA55FB0BBF81323D87062DB1F6D1C&dn=Movie&tr=udp://x"
    When its blocklist key is computed
    Then the blocklist key is "btih:dd8255ecdc7ca55fb0bbf81323d87062db1f6d1c"

  @C15 @passing
  Scenario: a .torrent-URL release is identified by indexer + normalised title + size, so proxy download tokens do not defeat the blocklist
    Given a release "Andy Weir - Project Hail Mary [M4B 128kbps]" of size 932188736 fetched by torrent URL "http://gluetun:9696/1/download?apikey=k&link=AAAA"
    When its blocklist key is computed
    Then the blocklist key equals that of a re-search yielding torrent URL "http://gluetun:9696/1/download?apikey=k&link=BBBB"
    And the blocklist key differs from that of the same title at size 700000000

  @C15 @passing
  Scenario: a magnet with no info hash falls back to the raw URI as its key
    Given a release "x" of size 1 fetched by magnet "magnet:?dn=nohash"
    When its blocklist key is computed
    Then the blocklist key is "magnet:?dn=nohash"

  @C15 @passing
  Scenario: an operator-pasted magnet becomes a manual release titled from its dn
    When the operator pastes the link "magnet:?xt=urn:btih:dd8255ecdc7ca55fb0bbf81323d87062db1f6d1c&dn=The.Matrix.1999.1080p" with no title
    Then a manual release titled "The.Matrix.1999.1080p" is built
    And the manual release fetch is a magnet

  @C15 @passing
  Scenario: an operator-pasted .torrent URL is titled from its filename unless a title is given
    When the operator pastes the link "https://tracker.test/dl/Heat.1995.1080p.torrent?token=x" with no title
    Then a manual release titled "Heat.1995.1080p" is built
    And the manual release fetch is a torrent URL
    When the operator pastes the link "https://tracker.test/dl/1" with title "Heat (1995)"
    Then a manual release titled "Heat (1995)" is built

  @C15 @passing
  Scenario: a magnet without an info hash or a non-URL is refused
    When the operator pastes the link "magnet:?dn=nohash" with no title
    Then no manual release is built
    When the operator pastes the link "ftp://x/y.torrent" with no title
    Then no manual release is built

  @C15 @passing
  Scenario Outline: search-title normalisation lowercases, folds diacritics, drops a leading "the" and collapses punctuation
    Then the title "<title>" normalises to "<normalised>"

    Examples:
      | title                     | normalised              |
      | The.Matrix.1999           | matrix 1999             |
      | Amélie                    | amelie                  |
      | Fast & Furious            | fast and furious        |
      | Spider-Man: No Way Home   | spider man no way home  |
      | All the President's Men   | all the president s men |

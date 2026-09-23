//! Recorded / hand-built provider responses used by the C15 scenarios. Everything
//! here is served by an in-process mock server — never a real indexer.

/// Look a named fixture body up (panics on an unknown name so a typo in a
/// feature file fails loudly).
pub fn body(name: &str) -> String {
    match name {
        "caps" => CAPS_XML.to_string(),
        "caps_tv" => CAPS_TV_XML.to_string(),
        "search_magnet" => SEARCH_MAGNET_XML.to_string(),
        "search_torrent_link" => SEARCH_TORRENT_LINK_XML.to_string(),
        "search_attr_size" => SEARCH_ATTR_SIZE_XML.to_string(),
        "search_enclosure_length" => SEARCH_ENCLOSURE_LENGTH_XML.to_string(),
        "search_infohash_only" => SEARCH_INFOHASH_ONLY_XML.to_string(),
        "search_no_link" => SEARCH_NO_LINK_XML.to_string(),
        "search_two_items" => SEARCH_TWO_ITEMS_XML.to_string(),
        "search_bad_date" => SEARCH_BAD_DATE_XML.to_string(),
        "empty_rss" => EMPTY_RSS_XML.to_string(),
        "challenge_html" => CHALLENGE_HTML.to_string(),
        "not_xml" => "this is not xml <<<".to_string(),
        "prowlarr_search" => PROWLARR_SEARCH_JSON.to_string(),
        "prowlarr_rss" => include_str!("../fixtures/prowlarr_rss_empty.json").to_string(),
        "prowlarr_indexers" => include_str!("../fixtures/prowlarr_indexer_list.json").to_string(),
        "prowlarr_empty" => "[]".to_string(),
        "prowlarr_no_title" => PROWLARR_NO_TITLE_JSON.to_string(),
        "tracker_json_rows" => TRACKER_JSON_ROWS.to_string(),
        "tracker_json_tv_rows" => TRACKER_JSON_TV_ROWS.to_string(),
        "tracker_torrent_link_rows" => TRACKER_TORRENT_LINK_ROWS.to_string(),
        "tracker_detail_rows" => TRACKER_DETAIL_ROWS.to_string(),
        "tracker_detail_page" => TRACKER_DETAIL_PAGE.to_string(),
        "tracker_detail_page_no_hash" => "<html><body>nothing here</body></html>".to_string(),
        "login_form" => r#"<form id="login" action="/takelogin.php"></form>"#.to_string(),
        "logged_in" => r#"<a class="logout">out</a>"#.to_string(),
        "not_logged_in" => r#"<a class="login">in</a>"#.to_string(),
        "solver_ok" => SOLVER_OK.to_string(),
        "solver_error" => SOLVER_ERROR.to_string(),
        other => panic!("unknown fixture {other:?}"),
    }
}

pub const CAPS_XML: &str = r#"<?xml version="1.0"?>
<caps>
  <searching>
    <search available="yes" supportedParams="q" />
    <movie-search available="yes" supportedParams="q,imdbid,tmdbid" />
  </searching>
  <categories>
    <category id="2000" name="Movies" />
    <category id="2040" name="Movies/HD" />
  </categories>
</caps>"#;

pub const CAPS_TV_XML: &str = r#"<?xml version="1.0"?>
<caps>
  <searching>
    <search available="yes" supportedParams="q" />
    <tv-search available="yes" supportedParams="q,tvdbid,season,ep" />
  </searching>
  <categories>
    <category id="5000" name="TV" />
    <category id="5040" name="TV/HD" />
  </categories>
</caps>"#;

pub const SEARCH_MAGNET_XML: &str = r#"<?xml version="1.0"?>
<rss xmlns:torznab="http://torznab.com/schemas/2015/feed">
  <channel>
    <item>
      <title>Blade.Runner.1982.2160p.UHD.BluRay.x265-GROUP</title>
      <link>magnet:?xt=urn:btih:deadbeefdeadbeefdeadbeefdeadbeefdeadbeef&amp;dn=Blade.Runner</link>
      <pubDate>Tue, 10 Jan 2023 12:00:00 +0000</pubDate>
      <size>15000000000</size>
      <torznab:attr name="seeders" value="42" />
      <torznab:attr name="peers" value="50" />
      <torznab:attr name="category" value="2000" />
      <torznab:attr name="category" value="2040" />
    </item>
  </channel>
</rss>"#;

pub const SEARCH_TORRENT_LINK_XML: &str = r#"<?xml version="1.0"?>
<rss xmlns:torznab="http://torznab.com/schemas/2015/feed">
  <channel>
    <item>
      <title>Heat.1995.1080p.BluRay.x264-GROUP</title>
      <link>http://proxy.test/1/download?apikey=k&amp;link=AAAA&amp;file=Heat.torrent</link>
      <enclosure url="http://proxy.test/1/download?apikey=k&amp;link=AAAA&amp;file=Heat.torrent" length="9000000000" type="application/x-bittorrent" />
      <pubDate>Tue, 10 Jan 2023 12:00:00 +0000</pubDate>
      <size>9000000000</size>
      <torznab:attr name="seeders" value="7" />
      <torznab:attr name="magneturl" value="magnet:?xt=urn:btih:cafebabecafebabecafebabecafebabecafebabe" />
      <torznab:attr name="category" value="2000" />
    </item>
  </channel>
</rss>"#;

/// No `<size>` element — only the `torznab:attr name="size"` form (Jackett
/// emits both; some Newznab servers only the attr).
pub const SEARCH_ATTR_SIZE_XML: &str = r#"<?xml version="1.0"?>
<rss xmlns:torznab="http://torznab.com/schemas/2015/feed">
  <channel>
    <item>
      <title>Heat.1995.1080p.BluRay.x264-GROUP</title>
      <link>magnet:?xt=urn:btih:cafebabecafebabecafebabecafebabecafebabe</link>
      <torznab:attr name="size" value="4200000000" />
      <torznab:attr name="category" value="2000" />
    </item>
  </channel>
</rss>"#;

/// Size only as the enclosure `length` (Newznab's original convention).
pub const SEARCH_ENCLOSURE_LENGTH_XML: &str = r#"<?xml version="1.0"?>
<rss xmlns:torznab="http://torznab.com/schemas/2015/feed">
  <channel>
    <item>
      <title>Heat.1995.1080p.BluRay.x264-GROUP</title>
      <link>http://proxy.test/dl/heat.torrent</link>
      <enclosure url="http://proxy.test/dl/heat.torrent" length="4200000000" type="application/x-bittorrent" />
      <torznab:attr name="category" value="2000" />
    </item>
  </channel>
</rss>"#;

/// No link/enclosure/magneturl at all — only an `infohash` attr (some trackers
/// via Jackett).
pub const SEARCH_INFOHASH_ONLY_XML: &str = r#"<?xml version="1.0"?>
<rss xmlns:torznab="http://torznab.com/schemas/2015/feed">
  <channel>
    <item>
      <title>Heat.1995.1080p.BluRay.x264-GROUP</title>
      <size>4200000000</size>
      <torznab:attr name="infohash" value="CAFEBABECAFEBABECAFEBABECAFEBABECAFEBABE" />
      <torznab:attr name="seeders" value="3" />
    </item>
  </channel>
</rss>"#;

pub const SEARCH_NO_LINK_XML: &str = r#"<?xml version="1.0"?>
<rss xmlns:torznab="http://torznab.com/schemas/2015/feed">
  <channel>
    <item>
      <title>Heat.1995.1080p.BluRay.x264-GROUP</title>
      <size>4200000000</size>
    </item>
    <item>
      <title></title>
      <link>magnet:?xt=urn:btih:cafebabecafebabecafebabecafebabecafebabe</link>
    </item>
  </channel>
</rss>"#;

pub const SEARCH_TWO_ITEMS_XML: &str = r#"<?xml version="1.0"?>
<rss xmlns:torznab="http://torznab.com/schemas/2015/feed">
  <channel>
    <item>
      <title>Lioness.S03E02.1080p.WEB-DL.h264-GRP</title>
      <link>magnet:?xt=urn:btih:0000000000000000000000000000000000000001</link>
      <pubDate>Tue, 10 Jan 2023 12:00:00 +0000</pubDate>
      <size>2000000000</size>
      <torznab:attr name="seeders" value="12" />
      <torznab:attr name="category" value="5040" />
    </item>
    <item>
      <title>Lioness.S03E01.720p.HDTV.x264-GRP</title>
      <link>magnet:?xt=urn:btih:0000000000000000000000000000000000000002</link>
      <pubDate>Mon, 09 Jan 2023 12:00:00 +0000</pubDate>
      <size>900000000</size>
      <torznab:attr name="seeders" value="0" />
      <torznab:attr name="category" value="5030" />
    </item>
  </channel>
</rss>"#;

pub const SEARCH_BAD_DATE_XML: &str = r#"<?xml version="1.0"?>
<rss xmlns:torznab="http://torznab.com/schemas/2015/feed">
  <channel>
    <item>
      <title>Heat.1995.1080p.BluRay.x264-GROUP</title>
      <link>magnet:?xt=urn:btih:cafebabecafebabecafebabecafebabecafebabe</link>
      <pubDate>not a date</pubDate>
      <size>abc</size>
      <torznab:attr name="seeders" value="lots" />
    </item>
  </channel>
</rss>"#;

pub const EMPTY_RSS_XML: &str = r#"<?xml version="1.0"?>
<rss xmlns:torznab="http://torznab.com/schemas/2015/feed"><channel></channel></rss>"#;

/// What CloudFlare serves in front of a protected tracker.
pub const CHALLENGE_HTML: &str = r#"<!DOCTYPE html><html><head><title>Just a moment...</title></head>
<body><div id="challenge-platform"><script>window._cf_chl_opt={}</script>Checking your browser before accessing the site.</div></body></html>"#;

pub const PROWLARR_SEARCH_JSON: &str = r#"[
  {"title":"Dungeon Crawler Carl (Audiobook) M4B","size":1200000000,"seeders":12,
   "downloadUrl":"http://prowlarr/15/download?link=abc","magnetUrl":null,
   "publishDate":"2026-05-12T00:00:00Z","indexer":"AudioBook Bay",
   "categories":[{"id":3030,"name":"Audio/Audiobook"}]},
  {"title":"Dungeon Crawler Carl [MP3 128]","size":500000000,"seeders":3,
   "downloadUrl":"http://prowlarr/8/download?link=def",
   "magnetUrl":"magnet:?xt=urn:btih:deadbeefdeadbeefdeadbeefdeadbeefdeadbeef","publishDate":"2025-01-01T00:00:00Z"}
]"#;

pub const PROWLARR_NO_TITLE_JSON: &str = r#"[
  {"title":"","size":1,"downloadUrl":"http://prowlarr/1/download?link=x"},
  {"size":1,"downloadUrl":"http://prowlarr/1/download?link=y"},
  {"title":"No fetch at all","size":1}
]"#;

pub const TRACKER_JSON_ROWS: &str = r#"[
  {"name":"The Matrix 1999 1080p BluRay x264","info_hash":"AAAABBBBAAAABBBBAAAABBBBAAAABBBBAAAABBBB","size":"1500000000","seeders":"42","category":"1","added":"1700000000"},
  {"name":"The Matrix 1999 720p BluRay x264","info_hash":"CCCCDDDDCCCCDDDDCCCCDDDDCCCCDDDDCCCCDDDD","size":"700000000","seeders":"5","category":"1","added":"1700000000"}
]"#;

pub const TRACKER_JSON_TV_ROWS: &str = r#"[
  {"name":"Lioness S03E02 1080p WEB-DL h264-GRP","info_hash":"EEEEFFFFEEEEFFFFEEEEFFFFEEEEFFFFEEEEFFFF","size":"2000000000","seeders":"12","category":"2","added":"1700000000"}
]"#;

pub const TRACKER_TORRENT_LINK_ROWS: &str = r#"[
  {"name":"Heat 1995 1080p BluRay x264","link":"/dl/1.torrent","size":"4200000000","seeders":"7","category":"1"}
]"#;

pub const TRACKER_DETAIL_ROWS: &str = r#"[
  {"name":"Andy Weir - Project Hail Mary [M4B]","details":"/detail/1","size":"932188736","seeders":"9","category":"3"}
]"#;

pub const TRACKER_DETAIL_PAGE: &str = r#"<html><body><table>
<tr><td>Info Hash:</td><td>0123456789abcdef0123456789abcdef01234567</td></tr>
</table></body></html>"#;

pub const SOLVER_OK: &str = r#"{"status":"ok","solution":{"status":200,
 "response":"[{\"name\":\"The Matrix 1999 1080p BluRay x264\",\"info_hash\":\"AAAABBBBAAAABBBBAAAABBBBAAAABBBBAAAABBBB\",\"size\":\"1500000000\",\"seeders\":\"42\",\"category\":\"1\",\"added\":\"1700000000\"}]",
 "userAgent":"Mozilla/5.0 Chrome/120","cookies":[{"name":"cf_clearance","value":"abc123"}]}}"#;

pub const SOLVER_ERROR: &str =
    r#"{"status":"error","message":"Error: Challenge not solved (timeout)"}"#;

/// Cardigann definitions used by the scenarios. `links` is patched to the mock
/// tracker at build time.
pub fn definition(name: &str) -> String {
    match name {
        "public_json" => PUBLIC_JSON_DEF.to_string(),
        "torrent_link" => TORRENT_LINK_DEF.to_string(),
        "detail_hash" => DETAIL_HASH_DEF.to_string(),
        "private_form" => PRIVATE_FORM_DEF.to_string(),
        "tv_only" => TV_ONLY_DEF.to_string(),
        other => panic!("unknown definition {other:?}"),
    }
}

pub const PUBLIC_JSON_DEF: &str = r#"
id: synthjson
name: SynthJSON
type: public
caps:
  categorymappings:
    - {id: "1", cat: Movies}
    - {id: "2", cat: TV}
    - {id: "3", cat: Audio/Audiobook}
  modes: {search: [q], movie-search: [q, imdbid], tv-search: [q, tvdbid, season, ep]}
search:
  paths:
    - path: 'search?q={{ .Keywords }}&cat={{ join .Categories "," }}&imdb={{ .Query.IMDBID }}&s={{ .Query.Season }}&e={{ .Query.Ep }}'
      response: {type: json}
  rows: {selector: "$"}
  fields:
    title: {selector: name}
    infohash: {selector: info_hash}
    size: {selector: size}
    seeders: {selector: seeders}
    category: {selector: category}
    date: {selector: added}
"#;

pub const TORRENT_LINK_DEF: &str = r#"
id: torrentlink
name: TorrentLink
type: public
caps:
  categorymappings:
    - {id: "1", cat: Movies}
  modes: {search: [q]}
search:
  paths:
    - path: 'search?q={{ .Keywords }}'
      response: {type: json}
  rows: {selector: "$"}
  fields:
    title: {selector: name}
    download: {selector: link}
    size: {selector: size}
    seeders: {selector: seeders}
    category: {selector: category}
"#;

pub const DETAIL_HASH_DEF: &str = r#"
id: detailhash
name: DetailHash
type: public
caps:
  categorymappings:
    - {id: "3", cat: Audio/Audiobook}
  modes: {search: [q]}
search:
  paths:
    - path: 'search?q={{ .Keywords }}'
      response: {type: json}
  rows: {selector: "$"}
  fields:
    title: {selector: name}
    download: {selector: details}
    size: {selector: size}
    seeders: {selector: seeders}
    category: {selector: category}
download:
  infohash:
    hash:
      selector: 'td:contains("Info Hash:") ~ td'
      filters:
        - name: regexp
          args: "([A-Fa-f0-9]{40})"
"#;

pub const PRIVATE_FORM_DEF: &str = r#"
id: priv
name: Priv
type: private
caps:
  categorymappings: [{id: "1", cat: Movies}]
  modes: {search: [q]}
login:
  path: login.php
  method: form
  form: form#login
  inputs:
    username: "{{ .Config.username }}"
    password: "{{ .Config.password }}"
  test:
    path: account.php
    selector: a.logout
search:
  paths:
    - path: "search.php?q={{ .Keywords }}"
      response: {type: json}
  rows: {selector: "$"}
  fields:
    title: {selector: name}
    infohash: {selector: hash}
    size: {selector: size}
"#;

pub const TV_ONLY_DEF: &str = r#"
id: tvonly
name: TVOnly
type: public
caps:
  categorymappings:
    - {id: "2", cat: TV}
  modes: {tv-search: [q, season, ep]}
search:
  paths:
    - path: 'search?q={{ .Keywords }}&cat={{ join .Categories "," }}'
      response: {type: json}
  rows: {selector: "$"}
  fields:
    title: {selector: name}
    infohash: {selector: info_hash}
    size: {selector: size}
    seeders: {selector: seeders}
    category: {selector: category}
"#;

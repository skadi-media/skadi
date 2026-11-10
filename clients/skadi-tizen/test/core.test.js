'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const C = require('../js/core.js');

test('serverUrl adds a scheme and drops trailing slashes and spaces', () => {
  assert.equal(C.serverUrl(' 192.0.2.10:8090/ '), 'http://192.0.2.10:8090');
  assert.equal(C.serverUrl('https://skadi.example/'), 'https://skadi.example');
  assert.equal(C.serverUrl(''), '');
});

test('videoUrl carries the token in the query, encoded', () => {
  assert.equal(
    C.videoUrl('http://192.0.2.10:8090/', 'm1', 'e1', 'a b&c'),
    'http://192.0.2.10:8090/api/v1/movies/m1/editions/e1/video?apikey=a%20b%26c'
  );
});

test('sizedImage swaps the TMDB size and leaves other URLs alone', () => {
  assert.equal(
    C.sizedImage('https://image.tmdb.org/t/p/original/abc.jpg', 'w342'),
    'https://image.tmdb.org/t/p/w342/abc.jpg'
  );
  assert.equal(C.sizedImage('https://example.org/p.jpg', 'w342'), 'https://example.org/p.jpg');
  assert.equal(C.sizedImage(null, 'w342'), '');
});

test('fmtTime', () => {
  assert.equal(C.fmtTime(7000), '0:07');
  assert.equal(C.fmtTime(754000), '12:34');
  assert.equal(C.fmtTime(3723000), '1:02:03');
  assert.equal(C.fmtTime(-5), '0:00');
});

test('fmtRuntime', () => {
  assert.equal(C.fmtRuntime(107), '1 h 47 min');
  assert.equal(C.fmtRuntime(45), '45 min');
  assert.equal(C.fmtRuntime(null), '');
});

test('seekTarget stays inside the file and off its last seconds', () => {
  assert.equal(C.seekTarget(10000, -30000, 600000), 0);
  assert.equal(C.seekTarget(10000, 30000, 600000), 40000);
  assert.equal(C.seekTarget(590000, 600000, 600000), 595000);
});

test('canDecodeAudio follows the 2017 TV list', () => {
  for (const c of ['aac', 'AC3', 'eac3', 'dts', 'mp3']) assert.equal(C.canDecodeAudio(c), true, c);
  for (const c of ['truehd', 'flac', 'opus']) assert.equal(C.canDecodeAudio(c), false, c);
  assert.equal(C.canDecodeAudio(''), true);
});

test('audioTracks reads AVPlay track info and skips other types', () => {
  const info = [
    { index: 0, type: 'VIDEO', extra_info: '{"fourCC":"h264"}' },
    { index: 1, type: 'AUDIO', extra_info: '{"language":"ru","channels":"6","fourCC":"ac3"}' },
    { index: 2, type: 'AUDIO', extra_info: 'not json' },
    { index: 3, type: 'TEXT', extra_info: '{}' },
  ];
  assert.deepEqual(C.audioTracks(info), [
    { index: 1, language: 'ru', codec: 'ac3', channels: 6 },
    { index: 2, language: '', codec: '', channels: 0 },
  ]);
});

test('pickAudio prefers the wanted language over the first track', () => {
  const tracks = [
    { index: 1, language: 'ru', codec: 'ac3' },
    { index: 2, language: 'uk', codec: 'ac3' },
    { index: 4, language: 'en', codec: 'dts' },
  ];
  assert.equal(C.pickAudio(tracks, 'en'), 2);
});

test('pickAudio skips a wanted-language track the TV cannot decode', () => {
  const tracks = [
    { index: 1, language: 'en', codec: 'truehd' },
    { index: 2, language: 'en', codec: 'ac3' },
  ];
  assert.equal(C.pickAudio(tracks, 'en'), 1);
});

test('pickAudio falls back to any decodable track, then to the first', () => {
  assert.equal(C.pickAudio([{ language: 'fr', codec: 'flac' }, { language: 'fr', codec: 'aac' }], 'en'), 1);
  assert.equal(C.pickAudio([{ language: 'fr', codec: 'flac' }], 'en'), 0);
  assert.equal(C.pickAudio([], 'en'), -1);
});

test('langMatches accepts two- and three-letter codes', () => {
  assert.equal(C.langMatches('eng', 'en'), true);
  assert.equal(C.langMatches('en-US', 'en'), true);
  assert.equal(C.langMatches('', 'en'), false);
  assert.equal(C.langMatches('ru', 'en'), false);
});

test('playable keeps only editions with a file', () => {
  assert.deepEqual(C.playable({ editions: [{ id: 'a' }, { id: 'b', file: { path: 'x' } }] }).map((e) => e.id), ['b']);
  assert.deepEqual(C.playable({}), []);
});

test('mediaSummary', () => {
  assert.equal(
    C.mediaSummary({ media_info: { video: { width: 3840, height: 2160, codec: 'hevc', dynamic_range: 'hdr10' }, audio: { codec: 'eac3', channels: 6 } } }),
    '4K HEVC HDR10 · EAC3 5.1'
  );
  assert.equal(C.mediaSummary({ media_info: { video: { width: 1920, height: 1036, codec: 'h264', dynamic_range: null }, audio: { codec: 'aac', channels: 2 } } }), '1080p H264 · AAC 2 ch');
  assert.equal(C.mediaSummary({}), '');
});

test('gridTop scrolls only as far as needed', () => {
  assert.equal(C.gridTop(0, 7, 2, 0), 0);
  assert.equal(C.gridTop(13, 7, 2, 0), 0); // row 1, visible
  assert.equal(C.gridTop(14, 7, 2, 0), 1); // row 2: scroll one
  assert.equal(C.gridTop(3, 7, 2, 4), 0); // jump back up
});

test('gridMove stops at the edges and reaches a short last row', () => {
  assert.equal(C.gridMove(0, 'left', 7, 20), 0);
  assert.equal(C.gridMove(6, 'right', 7, 20), 6);
  assert.equal(C.gridMove(3, 'up', 7, 20), 3);
  assert.equal(C.gridMove(3, 'down', 7, 20), 10);
  assert.equal(C.gridMove(13, 'down', 7, 20), 19); // row 2 has 6 items: land on the last
  assert.equal(C.gridMove(19, 'right', 7, 20), 19);
  assert.equal(C.gridMove(15, 'down', 7, 20), 15);
});

test('escapeHtml', () => {
  assert.equal(C.escapeHtml('<a href="x">&</a>'), '&lt;a href=&quot;x&quot;&gt;&amp;&lt;/a&gt;');
});

test('sizedImage uses the TVDB thumbnail for series art', () => {
  assert.equal(C.sizedImage('https://artworks.thetvdb.com/banners/posters/1-2.jpg', 'w342'), 'https://artworks.thetvdb.com/banners/posters/1-2_t.jpg');
  assert.equal(C.sizedImage('https://artworks.thetvdb.com/banners/posters/1-2_t.jpg', 'w342'), 'https://artworks.thetvdb.com/banners/posters/1-2_t.jpg');
});

test('episodeVideoUrl', () => {
  assert.equal(C.episodeVideoUrl('http://192.0.2.10:8090', 's1', 'e1', 'tok'), 'http://192.0.2.10:8090/api/v1/series/s1/episodes/e1/video?apikey=tok');
});

const titled = (t, s) => ({ title: t, sort_title: s });

test('letterOf uses the sort title and files digits and symbols under #', () => {
  assert.equal(C.letterOf(titled('The Abyss', 'abyss')), 'A');
  assert.equal(C.letterOf(titled('2001', '2001')), '#');
  assert.equal(C.letterOf(titled('*batteries not included', '*batteries not included')), '#');
  assert.equal(C.letterOf({ title: 'Zodiac' }), 'Z');
});

test('letterIndex gives the first position of each letter present', () => {
  const items = [titled('12', '12'), titled('Alien', 'alien'), titled('Aliens', 'aliens'), titled('Dune', 'dune')];
  assert.deepEqual(C.letterIndex(items), { '#': 0, A: 1, D: 3 });
});

test('nextLetter skips letters with no titles and stops at the ends', () => {
  const index = { '#': 0, A: 1, D: 3 };
  assert.equal(C.nextLetter(index, 'A', 1), 'D');
  assert.equal(C.nextLetter(index, 'D', 1), 'D');
  assert.equal(C.nextLetter(index, 'D', -1), 'A');
  assert.equal(C.nextLetter(index, 'B', -1), 'A');
});

test('search matches word beginnings in any order, title starts first', () => {
  const items = ['Star Wars', 'The War of the Stars', 'Stargate', 'Warcraft', 'Starship Troopers'].map((t) => ({ title: t }));
  assert.deepEqual(C.search(items, 'st wa').map((i) => i.title), ['Star Wars', 'The War of the Stars']);
  assert.deepEqual(C.search(items, 'star').map((i) => i.title), ['Star Wars', 'Stargate', 'Starship Troopers', 'The War of the Stars']);
  assert.deepEqual(C.search(items, 'ar'), []); // not the start of a word
  assert.deepEqual(C.search(items, '  '), []);
  assert.equal(C.search(items, 'star', 2).length, 2);
});

test('search ignores punctuation and case', () => {
  assert.equal(C.search([{ title: "Schindler's List" }], 'SCHINDLERS').length, 0);
  assert.equal(C.search([{ title: "Schindler's List" }], 'schindler s l').length, 1);
  assert.equal(C.search([{ title: 'WALL·E' }], 'wall e').length, 1);
});

test('recent keeps one row per title, newest first, without upgrades', () => {
  const rows = [
    { kind: 'series', item_id: 'a', episode: 'S01E03' },
    { kind: 'series', item_id: 'a', episode: 'S01E02' },
    { kind: 'movie', item_id: 'm', upgrade: true },
    { kind: 'movie', item_id: 'a' },
    { kind: 'series', item_id: 'b' },
  ];
  assert.deepEqual(C.recent(rows).map((r) => r.kind + r.item_id + (r.episode || '')), ['seriesaS01E03', 'moviea', 'seriesb']);
  assert.equal(C.recent(rows, 2).length, 2);
  assert.deepEqual(C.recent(null), []);
});

const show = {
  episodes: [
    { season: 0, number: 1, file: {} },
    { season: 2, number: 1, file: {} },
    { season: 1, number: 2, file: {} },
    { season: 1, number: 1, file: {} },
    { season: 1, number: 3 },
    { season: 3, number: 1 },
  ],
};

test('episodeCode', () => {
  assert.equal(C.episodeCode({ season: 1, number: 2 }), 'S01E02');
  assert.equal(C.episodeCode({ season: 12, number: 110 }), 'S12E110');
});

test('seasons lists the ones with files, specials last', () => {
  assert.deepEqual(C.seasons(show), [1, 2, 0]);
  assert.deepEqual(C.seasons({}), []);
});

test('episodesOf keeps episodes without a file, in order', () => {
  assert.deepEqual(C.episodesOf(show, 1).map((e) => [e.number, !!e.file]), [[1, true], [2, true], [3, false]]);
});

test('nextEpisode steps over missing episodes and into the next season', () => {
  assert.deepEqual(C.nextEpisode(show, { season: 1, number: 1 }).number, 2);
  const n = C.nextEpisode(show, { season: 1, number: 2 });
  assert.deepEqual([n.season, n.number], [2, 1]);
  assert.equal(C.nextEpisode(show, { season: 2, number: 1 }), null);
  assert.equal(C.nextEpisode(show, { season: 0, number: 1 }).season, 1);
});

test('importedCount reads the summary view', () => {
  assert.equal(C.importedCount({ episodes: [{ status: 'Imported' }, { status: 'Missing' }, { status: 'Failed' }] }), 1);
});

test('skipStep grows while the key is held', () => {
  assert.equal(C.skipStep(0), 10000);
  assert.equal(C.skipStep(6), 30000);
  assert.equal(C.skipStep(16), 60000);
});

test('rowLeft scrolls a row only as far as needed', () => {
  assert.equal(C.rowLeft(0, 6, 0), 0);
  assert.equal(C.rowLeft(5, 6, 0), 0);
  assert.equal(C.rowLeft(6, 6, 0), 1);
  assert.equal(C.rowLeft(2, 6, 4), 2);
});

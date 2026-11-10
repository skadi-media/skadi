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

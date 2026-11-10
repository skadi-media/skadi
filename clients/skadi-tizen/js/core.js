// Pure logic for the TV app: no DOM, no network, no player.
//
// ES5 on purpose. The 2017 Samsung sets run Chromium M47: no modules, no
// async/await, no arrow functions. The same file loads in the TV (as a script
// tag, filling `window.SkadiCore`) and in Node (for `node --test`).
(function (root, factory) {
  if (typeof module === 'object' && module.exports) module.exports = factory();
  else root.SkadiCore = factory();
})(this, function () {
  'use strict';

  // Audio the TV decodes, by the names the server's media info uses and the
  // names AVPlay reports. From Samsung's 2017 video specification; TrueHD,
  // FLAC and Opus are not on it.
  var TV_AUDIO = ['aac', 'ac3', 'eac3', 'dts', 'mp3', 'mp2', 'vorbis', 'pcm', 'lpcm'];

  function trimSlash(s) {
    return String(s || '').replace(/\s+/g, '').replace(/\/+$/, '');
  }

  // "http://host:8090" from what a person types with a TV remote.
  function serverUrl(typed) {
    var s = trimSlash(typed);
    if (!s) return '';
    if (!/^https?:\/\//i.test(s)) s = 'http://' + s;
    return s;
  }

  function apiUrl(server, path) {
    return trimSlash(server) + '/api/v1' + path;
  }

  // AVPlay takes a URL and nothing else, so the token rides in the query.
  function videoUrl(server, movieId, editionId, token) {
    return apiUrl(server, '/movies/' + movieId + '/editions/' + editionId + '/video') +
      '?apikey=' + encodeURIComponent(token);
  }

  // The server stores TMDB's full-size image URL. A grid of those would pull
  // megabytes per poster; TMDB serves fixed smaller widths from the same path.
  // TVDB (series art) has one small size: the same name with `_t` before the
  // extension. Any other host's URL is used as it is.
  function sizedImage(url, width) {
    if (!url) return '';
    var u = String(url);
    if (u.indexOf('/t/p/original/') >= 0) return u.replace('/t/p/original/', '/t/p/' + width + '/');
    if (u.indexOf('//artworks.thetvdb.com/') >= 0 && !/_t\.[a-z]+$/i.test(u)) return u.replace(/(\.[a-z]+)$/i, '_t$1');
    return u;
  }

  function episodeVideoUrl(server, seriesId, episodeId, token) {
    return apiUrl(server, '/series/' + seriesId + '/episodes/' + episodeId + '/video') +
      '?apikey=' + encodeURIComponent(token);
  }

  function pad2(n) {
    return (n < 10 ? '0' : '') + n;
  }

  // 0:07, 12:34, 1:02:03
  function fmtTime(ms) {
    var s = Math.max(0, Math.floor((ms || 0) / 1000));
    var h = Math.floor(s / 3600), m = Math.floor((s % 3600) / 60), sec = s % 60;
    return h > 0 ? h + ':' + pad2(m) + ':' + pad2(sec) : m + ':' + pad2(sec);
  }

  function fmtRuntime(minutes) {
    if (!minutes) return '';
    var h = Math.floor(minutes / 60), m = minutes % 60;
    return h > 0 ? h + ' h ' + m + ' min' : m + ' min';
  }

  // Where a seek lands: inside the file, and not on its last seconds, where
  // the player would end the stream at once.
  function seekTarget(currentMs, deltaMs, durationMs) {
    var end = Math.max(0, (durationMs || 0) - 5000);
    return Math.max(0, Math.min(end, currentMs + deltaMs));
  }

  function canDecodeAudio(codec) {
    if (!codec) return true; // unknown: let the player try
    var c = String(codec).toLowerCase();
    for (var i = 0; i < TV_AUDIO.length; i++) if (c.indexOf(TV_AUDIO[i]) === 0) return true;
    return false;
  }

  // "en" matches "en", "eng" and "en-US". An empty language matches nothing.
  function langMatches(lang, want) {
    if (!lang || !want) return false;
    return String(lang).toLowerCase().indexOf(String(want).toLowerCase()) === 0;
  }

  // The audio tracks out of AVPlay's getTotalTrackInfo(): each entry has an
  // index, a type and `extra_info`, a JSON string.
  function audioTracks(trackInfo) {
    var out = [];
    for (var i = 0; i < (trackInfo || []).length; i++) {
      var t = trackInfo[i];
      if (t.type !== 'AUDIO') continue;
      var x = {};
      try { x = JSON.parse(t.extra_info) || {}; } catch (e) { x = {}; }
      out.push({
        index: t.index,
        language: x.language || '',
        codec: x.fourCC || '',
        channels: parseInt(x.channels, 10) || 0
      });
    }
    return out;
  }

  // Which track to play. A film's first track is not always the one wanted
  // (seen: Russian first, English fourth), and not every track decodes.
  // Preference: wanted language and decodable, then any decodable, then the
  // first. Returns a position in `tracks`, or -1 when there are none.
  function pickAudio(tracks, wantLang) {
    var i;
    if (!tracks || !tracks.length) return -1;
    for (i = 0; i < tracks.length; i++) {
      if (langMatches(tracks[i].language, wantLang) && canDecodeAudio(tracks[i].codec)) return i;
    }
    for (i = 0; i < tracks.length; i++) if (canDecodeAudio(tracks[i].codec)) return i;
    return 0;
  }

  function audioLabel(t) {
    if (!t) return 'Audio';
    var parts = [];
    if (t.language) parts.push(t.language.toUpperCase());
    if (t.codec) parts.push(t.codec.toUpperCase());
    if (t.channels) parts.push(t.channels === 6 ? '5.1' : t.channels === 8 ? '7.1' : t.channels + ' ch');
    return parts.length ? parts.join(' ') : 'Track ' + t.index;
  }

  // Editions of a movie that have a file on disk.
  function playable(movie) {
    var out = [], eds = (movie && movie.editions) || [];
    for (var i = 0; i < eds.length; i++) if (eds[i].file) out.push(eds[i]);
    return out;
  }

  // "4K HEVC HDR10 · EAC3 5.1" from the server's media info.
  function mediaSummary(edition) {
    var m = edition && edition.media_info;
    if (!m) return '';
    var parts = [], v = m.video, a = m.audio;
    if (v) {
      var res = v.width >= 3800 ? '4K' : v.width >= 1900 ? '1080p' : v.width >= 1260 ? '720p' : (v.height ? v.height + 'p' : '');
      var vp = [];
      if (res) vp.push(res);
      if (v.codec) vp.push(String(v.codec).toUpperCase());
      if (v.dynamic_range && v.dynamic_range !== 'sdr') vp.push(String(v.dynamic_range).toUpperCase());
      if (vp.length) parts.push(vp.join(' '));
    }
    if (a && a.codec) parts.push(audioLabel({ codec: a.codec, channels: a.channels }));
    return parts.join(' · ');
  }

  // The grid draws only the rows on screen. Given the selected item, the
  // column count, how many rows fit and the first row drawn now, return the
  // first row to draw so the selection is visible, moving as little as possible.
  function gridTop(selected, cols, rowsVisible, top) {
    var row = Math.floor(selected / cols);
    if (row < top) return row;
    if (row >= top + rowsVisible) return row - rowsVisible + 1;
    return top;
  }

  // Move in a grid of `count` items with `cols` columns. Stays put at an
  // edge; moving down from the last full row lands on the last item.
  function gridMove(selected, key, cols, count) {
    var next = selected;
    if (key === 'left') next = selected % cols === 0 ? selected : selected - 1;
    else if (key === 'right') next = (selected % cols === cols - 1) ? selected : selected + 1;
    else if (key === 'up') next = selected - cols;
    else if (key === 'down') next = selected + cols;
    if (next < 0) return selected;
    if (next >= count) {
      var lastRow = Math.floor((count - 1) / cols);
      if (key === 'down' && Math.floor(selected / cols) < lastRow) return count - 1;
      return selected;
    }
    return next;
  }

  // ---- Finding a title ----------------------------------------------------

  // The key a title sorts and jumps by. The server sends `sort_title`
  // (lowercase, article dropped); the title is the fallback.
  function sortKey(item) {
    return String((item && (item.sort_title || item.title)) || '').toLowerCase();
  }
  function byTitle(a, b) {
    var x = sortKey(a), y = sortKey(b);
    return x < y ? -1 : x > y ? 1 : 0;
  }
  // 'A'..'Z', or '#' for a digit, a symbol or nothing.
  function letterOf(item) {
    var c = sortKey(item).charAt(0).toUpperCase();
    return c >= 'A' && c <= 'Z' ? c : '#';
  }
  var LETTERS = '#ABCDEFGHIJKLMNOPQRSTUVWXYZ'.split('');
  // For a list sorted by title: the position of the first item under each
  // letter. A letter with no titles is absent.
  function letterIndex(items) {
    var out = {};
    for (var i = 0; i < items.length; i++) {
      var l = letterOf(items[i]);
      if (!(l in out)) out[l] = i;
    }
    return out;
  }
  // The nearest letter that has titles, looking from `from` in direction
  // `step` (1 or -1). Returns `from` when there is none that way.
  function nextLetter(index, from, step) {
    var i = LETTERS.indexOf(from);
    for (i += step; i >= 0 && i < LETTERS.length; i += step) if (LETTERS[i] in index) return LETTERS[i];
    return from;
  }

  function plain(s) {
    return String(s || '').toLowerCase().replace(/[^a-z0-9]+/g, ' ').replace(/^ +| +$/g, '');
  }
  // Titles that match what was typed on the TV's letter grid. Every typed
  // word must begin a word of the title, in any order, so "st wa" finds
  // "Star Wars". Titles that begin with the query come first.
  function search(items, query, limit) {
    var q = plain(query), first = [], rest = [], i, j;
    if (!q) return [];
    var words = q.split(' ');
    for (i = 0; i < items.length; i++) {
      var t = ' ' + plain(items[i].title), ok = true;
      for (j = 0; j < words.length && ok; j++) ok = t.indexOf(' ' + words[j]) >= 0;
      if (!ok) continue;
      (t.indexOf(' ' + q) === 0 ? first : rest).push(items[i]);
    }
    var out = first.concat(rest);
    return limit ? out.slice(0, limit) : out;
  }

  // ---- Recently added ------------------------------------------------------

  // The new-imports feed has one row per file, so a season that arrived
  // tonight is twenty rows. Keep the first row of each movie or series, in
  // feed order (newest first), and leave upgrades of older files out.
  function recent(rows, limit) {
    var seen = {}, out = [];
    for (var i = 0; i < (rows || []).length; i++) {
      var r = rows[i], key = r.kind + ':' + r.item_id;
      if (r.upgrade || seen[key]) continue;
      seen[key] = true;
      out.push(r);
      if (limit && out.length >= limit) break;
    }
    return out;
  }

  // ---- Series --------------------------------------------------------------

  function pad2n(n) { return (n < 10 ? '0' : '') + n; }
  function episodeCode(ep) {
    return 'S' + pad2n(ep.season) + 'E' + pad2n(ep.number);
  }
  // Season numbers that have at least one file, in order, specials (0) last.
  function seasons(series) {
    var have = {}, out = [], eps = (series && series.episodes) || [];
    for (var i = 0; i < eps.length; i++) if (eps[i].file && !have[eps[i].season]) { have[eps[i].season] = true; out.push(eps[i].season); }
    out.sort(function (a, b) { return (a === 0) - (b === 0) || a - b; });
    return out;
  }
  // The episodes of a season in order. Ones with no file stay in the list
  // (the page shows them greyed), so a gap in a season is visible.
  function episodesOf(series, season) {
    var out = [], eps = (series && series.episodes) || [];
    for (var i = 0; i < eps.length; i++) if (eps[i].season === season) out.push(eps[i]);
    out.sort(function (a, b) { return a.number - b.number; });
    return out;
  }
  // The next episode with a file after `ep`: later in its season, then the
  // first of a later season. Specials are never "next". null at the end.
  function nextEpisode(series, ep) {
    var all = [], eps = (series && series.episodes) || [];
    for (var i = 0; i < eps.length; i++) if (eps[i].file && eps[i].season > 0) all.push(eps[i]);
    all.sort(function (a, b) { return a.season - b.season || a.number - b.number; });
    for (i = 0; i < all.length; i++) {
      if (all[i].season > ep.season || (all[i].season === ep.season && all[i].number > ep.number)) return all[i];
    }
    return null;
  }
  // How many episodes of a summary-view series have a file.
  function importedCount(series) {
    var n = 0, eps = (series && series.episodes) || [];
    for (var i = 0; i < eps.length; i++) if (eps[i].status === 'Imported' || eps[i].file) n++;
    return n;
  }

  // ---- Skipping with the arrows --------------------------------------------

  // How far one more press of Left or Right skips. A held key repeats: the
  // step grows so a long hold crosses a film in seconds, not minutes.
  function skipStep(repeats) {
    return repeats < 6 ? 10000 : repeats < 16 ? 30000 : 60000;
  }

  // A horizontal row draws `visible` tiles. Return the first tile to draw so
  // the selected one shows, moving as little as possible.
  function rowLeft(selected, visible, left) {
    if (selected < left) return selected;
    if (selected >= left + visible) return selected - visible + 1;
    return left;
  }

  function escapeHtml(s) {
    return String(s == null ? '' : s)
      .replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
  }

  return {
    serverUrl: serverUrl, apiUrl: apiUrl, videoUrl: videoUrl, sizedImage: sizedImage,
    fmtTime: fmtTime, fmtRuntime: fmtRuntime, seekTarget: seekTarget,
    canDecodeAudio: canDecodeAudio, langMatches: langMatches, audioTracks: audioTracks,
    pickAudio: pickAudio, audioLabel: audioLabel, playable: playable, mediaSummary: mediaSummary,
    gridTop: gridTop, gridMove: gridMove, escapeHtml: escapeHtml,
    episodeVideoUrl: episodeVideoUrl, sortKey: sortKey, byTitle: byTitle, letterOf: letterOf, LETTERS: LETTERS,
    letterIndex: letterIndex, nextLetter: nextLetter, search: search, recent: recent,
    episodeCode: episodeCode, seasons: seasons, episodesOf: episodesOf, nextEpisode: nextEpisode,
    importedCount: importedCount, skipStep: skipStep, rowLeft: rowLeft
  };
});

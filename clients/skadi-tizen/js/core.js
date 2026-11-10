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
  function sizedImage(url, width) {
    if (!url) return '';
    return String(url).replace('/t/p/original/', '/t/p/' + width + '/');
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

  function escapeHtml(s) {
    return String(s == null ? '' : s)
      .replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
  }

  return {
    serverUrl: serverUrl, apiUrl: apiUrl, videoUrl: videoUrl, sizedImage: sizedImage,
    fmtTime: fmtTime, fmtRuntime: fmtRuntime, seekTarget: seekTarget,
    canDecodeAudio: canDecodeAudio, langMatches: langMatches, audioTracks: audioTracks,
    pickAudio: pickAudio, audioLabel: audioLabel, playable: playable, mediaSummary: mediaSummary,
    gridTop: gridTop, gridMove: gridMove, escapeHtml: escapeHtml
  };
});

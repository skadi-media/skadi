// The TV app: four views (sign in, movies, movie page, player) and the
// remote-control keys that drive them. ES5; see core.js for why.
(function () {
  'use strict';
  var C = window.SkadiCore;
  // A developer's own defaults and hooks, from the uncommitted js/dev.local.js.
  var DEV = window.SKADI_DEV || {};

  var KEY = {
    LEFT: 37, UP: 38, RIGHT: 39, DOWN: 40, ENTER: 13, BACK: 10009,
    PLAY: 415, PAUSE: 19, PLAYPAUSE: 10252, STOP: 413, FF: 417, RW: 412,
    IME_DONE: 65376, IME_CANCEL: 65385
  };
  var COLS = 7, ROWS = 2;
  var PREF_LANG = 'en';
  var BAR_HIDE_MS = 5000, SEEK_TIMEOUT_MS = 10000;

  var S = {
    view: 'setup',
    server: localStorage.getItem('server') || DEV.server || '',
    token: localStorage.getItem('token') || '',
    movies: [], sel: 0, top: 0,
    movie: null, editions: [], action: 0
  };

  function $(id) { return document.getElementById(id); }
  function log(msg) {
    try { console.log(msg); } catch (e) {}
    if (DEV.log) { try { DEV.log(String(msg)); } catch (e2) {} }
  }
  function errText(e) { return (e && (e.name || e.message)) || String(e); }
  window.onerror = function (m, f, l) { log('JS ERROR ' + m + ' @' + l); };

  var toastTimer = null;
  function toast(msg, info) {
    var t = $('toast');
    t.textContent = msg; t.className = info ? 'on info' : 'on';
    if (toastTimer) clearTimeout(toastTimer);
    toastTimer = setTimeout(function () { t.className = ''; }, 6000);
    log('toast: ' + msg);
  }

  function show(view) {
    var names = ['setup', 'browse', 'detail', 'player'];
    for (var i = 0; i < names.length; i++) $(names[i]).className = 'view' + (names[i] === view ? ' on' : '');
    document.body.className = view === 'player' ? 'playing' : '';
    S.view = view;
  }

  function api(method, path, body, done) {
    var x = new XMLHttpRequest();
    x.open(method, C.apiUrl(S.server, path), true);
    x.timeout = 20000;
    x.setRequestHeader('Content-Type', 'application/json');
    if (S.token) x.setRequestHeader('Authorization', 'Bearer ' + S.token);
    x.onreadystatechange = function () {
      if (x.readyState !== 4) return;
      var json = null;
      try { json = JSON.parse(x.responseText); } catch (e) {}
      done(x.status, json);
    };
    x.send(body ? JSON.stringify(body) : null);
  }

  // ---- Sign in -----------------------------------------------------------

  var FIELDS = ['server', 'user', 'pass', 'go'], field = 0;
  function fieldInput(name) { return $('f-' + name); }
  function drawSetup() {
    var rows = document.querySelectorAll('#setup .field');
    for (var i = 0; i < rows.length; i++) rows[i].className = 'field' + (i === field ? ' sel' : '');
  }
  function openSetup() {
    $('f-server').value = S.server;
    if (!$('f-user').value && DEV.user) $('f-user').value = DEV.user;
    field = S.server ? ($('f-user').value ? 2 : 1) : 0;
    show('setup'); drawSetup();
  }
  function signIn() {
    S.server = C.serverUrl($('f-server').value);
    if (!S.server) { toast('Enter the server address'); return; }
    toast('Signing in…', true);
    S.token = '';
    api('POST', '/auth/login', {
      username: $('f-user').value, password: $('f-pass').value, label: 'Samsung TV'
    }, function (status, json) {
      if (status === 200 && json && json.token) {
        S.token = json.token;
        localStorage.setItem('server', S.server); localStorage.setItem('token', S.token);
        $('f-pass').value = '';
        loadMovies();
      } else if (status === 401 || status === 403) toast('That username and password do not match');
      else if (status === 0) toast('Cannot reach ' + S.server);
      else toast('Sign in failed (' + status + ')');
    });
  }
  function keySetup(k) {
    var typing = document.activeElement && document.activeElement.tagName === 'INPUT';
    if (typing) {
      // The on-screen keyboard owns the keys until it is closed.
      if (k === KEY.IME_DONE || k === KEY.IME_CANCEL || k === KEY.BACK) document.activeElement.blur();
      return;
    }
    if (k === KEY.UP) field = Math.max(0, field - 1);
    else if (k === KEY.DOWN) field = Math.min(FIELDS.length - 1, field + 1);
    else if (k === KEY.ENTER) {
      if (FIELDS[field] === 'go') signIn(); else fieldInput(FIELDS[field]).focus();
    } else if (k === KEY.PLAY || k === KEY.PLAYPAUSE) signIn();
    else if (k === KEY.BACK) exitApp();
    drawSetup();
  }

  // ---- Movies ------------------------------------------------------------

  function loadMovies() {
    toast('Loading…', true);
    api('GET', '/movies', null, function (status, json) {
      if (status === 401) { S.token = ''; localStorage.removeItem('token'); toast('Signed out. Sign in again.'); openSetup(); return; }
      if (status !== 200 || !json) { toast(status === 0 ? 'Cannot reach ' + S.server : 'Could not load movies (' + status + ')'); openSetup(); return; }
      S.movies = [];
      for (var i = 0; i < json.length; i++) if (C.playable(json[i]).length) S.movies.push(json[i]);
      S.sel = Math.min(S.sel, Math.max(0, S.movies.length - 1)); S.top = 0;
      $('toast').className = '';
      show('browse'); drawGrid();
    });
  }
  function drawGrid() {
    S.top = C.gridTop(S.sel, COLS, ROWS, S.top);
    var html = '', first = S.top * COLS, last = Math.min(S.movies.length, first + COLS * (ROWS + 1));
    for (var i = first; i < last; i++) {
      var m = S.movies[i], img = C.sizedImage(m.poster_url, 'w342');
      html += '<div class="tile' + (i === S.sel ? ' sel' : '') + '"><div class="art">' +
        (img ? '<img src="' + C.escapeHtml(img) + '" alt="">' : '<div class="blank">' + C.escapeHtml(m.title) + '</div>') +
        '</div><div class="name">' + C.escapeHtml(m.title) + '</div></div>';
    }
    $('grid').innerHTML = html;
    var cur = S.movies[S.sel];
    $('browse-count').textContent = S.movies.length ? (S.sel + 1) + ' of ' + S.movies.length : 'No movies';
    $('browse-title').textContent = cur ? cur.title + (cur.year ? ' (' + cur.year + ')' : '') : '';
  }
  function keyBrowse(k) {
    var dir = k === KEY.LEFT ? 'left' : k === KEY.RIGHT ? 'right' : k === KEY.UP ? 'up' : k === KEY.DOWN ? 'down' : '';
    if (dir) { S.sel = C.gridMove(S.sel, dir, COLS, S.movies.length); drawGrid(); }
    else if (k === KEY.FF) { S.sel = Math.min(S.movies.length - 1, S.sel + COLS * ROWS * 3); drawGrid(); }
    else if (k === KEY.RW) { S.sel = Math.max(0, S.sel - COLS * ROWS * 3); drawGrid(); }
    else if (k === KEY.ENTER && S.movies[S.sel]) openDetail(S.movies[S.sel]);
    else if (k === KEY.BACK) exitApp();
  }

  // ---- Movie page --------------------------------------------------------

  function openDetail(movie) {
    S.movie = movie; S.editions = C.playable(movie); S.action = 0;
    $('detail-title').textContent = movie.title;
    var meta = [];
    if (movie.year) meta.push(movie.year);
    if (movie.content_rating) meta.push(movie.content_rating);
    if (movie.runtime_minutes) meta.push(C.fmtRuntime(movie.runtime_minutes));
    if (movie.genres && movie.genres.length) meta.push(movie.genres.slice(0, 3).join(', '));
    $('detail-meta').textContent = meta.join('  ·  ');
    $('detail-overview').textContent = movie.overview || '';
    $('detail-poster').src = C.sizedImage(movie.poster_url, 'w500');
    var bg = C.sizedImage(movie.backdrop_url, 'w1280');
    $('detail-backdrop').style.backgroundImage = bg ? 'url("' + bg.replace(/"/g, '%22') + '")' : 'none';
    show('detail'); drawActions();
  }
  function drawActions() {
    var html = '';
    for (var i = 0; i < S.editions.length; i++) {
      html += '<span class="action' + (i === S.action ? ' sel' : '') + '">▶ Play<small>' +
        C.escapeHtml(C.mediaSummary(S.editions[i]) || 'File ' + (i + 1)) + '</small></span>';
    }
    $('detail-actions').innerHTML = html;
  }
  function keyDetail(k) {
    if (k === KEY.LEFT) S.action = Math.max(0, S.action - 1);
    else if (k === KEY.RIGHT) S.action = Math.min(S.editions.length - 1, S.action + 1);
    else if (k === KEY.ENTER || k === KEY.PLAY || k === KEY.PLAYPAUSE) { play(S.movie, S.editions[S.action]); return; }
    else if (k === KEY.BACK) { show('browse'); drawGrid(); return; }
    drawActions();
  }

  // ---- Player ------------------------------------------------------------

  var P = { ready: false, seeking: false, tracks: [], track: -1, button: 2, tick: null, hide: null, seekTimer: null };
  var BUTTONS = [
    { label: '−10 min', seek: -600000 }, { label: '−30 s', seek: -30000 },
    { label: 'Pause', toggle: true },
    { label: '+30 s', seek: 30000 }, { label: '+10 min', seek: 600000 },
    { label: 'Audio', audio: true }
  ];
  function av() { return window.webapis && window.webapis.avplay; }
  function avState() { try { return av().getState(); } catch (e) { return 'NONE'; } }

  function play(movie, edition) {
    if (!av()) { toast('This device has no Samsung player'); return; }
    var info = edition.media_info || {};
    var url = DEV.videoUrl ? DEV.videoUrl(S.server, movie.id, edition.id, S.token, info.container || '')
      : C.videoUrl(S.server, movie.id, edition.id, S.token);
    P.ready = false; P.seeking = false; P.tracks = []; P.track = -1; P.button = 2;
    $('bar-title').textContent = movie.title;
    $('player-wait').style.display = 'block';
    show('player'); showBar();
    log('play ' + movie.title + ' [' + C.mediaSummary(edition) + ']');
    try {
      try { av().close(); } catch (e0) {}
      av().open(url);
      av().setDisplayRect(0, 0, 1920, 1080);
      av().setListener({
        onbufferingstart: function () { if (P.ready) $('player-wait').style.display = 'block'; },
        onbufferingcomplete: function () { $('player-wait').style.display = 'none'; },
        onstreamcompleted: function () { log('stream completed'); stop(); },
        onerror: function (e) { log('player error ' + e); toast('The TV could not play this file (' + e + ')'); stop(); },
        onevent: function () {}, oncurrentplaytime: function () {}, onsubtitlechange: function () {}
      });
      av().prepareAsync(function () {
        if (S.view !== 'player') return; // Back was pressed while it loaded
        P.ready = true;
        $('player-wait').style.display = 'none';
        av().play();
        chooseAudio();
        P.tick = setInterval(drawBar, 1000);
        showBar();
      }, function (e) {
        log('prepare failed ' + errText(e));
        toast('The TV could not open this file');
        stop();
      });
    } catch (e) {
      log('open failed ' + errText(e));
      toast('The TV could not open this file');
      stop();
    }
  }

  // The first track is not always the right one. Pick by language and by
  // what the TV decodes; leave the player's own choice alone when it agrees.
  function chooseAudio() {
    try { P.tracks = C.audioTracks(av().getTotalTrackInfo()); } catch (e) { P.tracks = []; log('track info failed ' + errText(e)); }
    var want = C.pickAudio(P.tracks, PREF_LANG);
    P.track = P.tracks.length ? 0 : -1;
    for (var i = 0; i < P.tracks.length; i++) log('audio ' + i + ': ' + C.audioLabel(P.tracks[i]));
    if (want > 0) selectAudio(want);
    if (P.tracks.length && !C.canDecodeAudio(P.tracks[P.track].codec)) toast('This TV cannot play this file’s sound (' + P.tracks[P.track].codec.toUpperCase() + ')');
  }
  function selectAudio(pos) {
    try {
      av().setSelectTrack('AUDIO', P.tracks[pos].index);
      P.track = pos;
      log('audio track -> ' + C.audioLabel(P.tracks[pos]));
    } catch (e) { log('audio switch failed ' + errText(e)); toast('Could not switch the audio track'); }
  }

  function stop() {
    if (P.tick) { clearInterval(P.tick); P.tick = null; }
    if (P.hide) { clearTimeout(P.hide); P.hide = null; }
    if (P.seekTimer) { clearTimeout(P.seekTimer); P.seekTimer = null; }
    P.ready = false; P.seeking = false;
    try { av().stop(); } catch (e) {}
    try { av().close(); } catch (e2) {}
    if (S.view === 'player') { show('detail'); drawActions(); }
  }

  // One seek at a time. Samsung's player refuses any call made while a seek
  // is running, and does not always answer one: the timer frees the bar.
  function seek(deltaMs) {
    if (!P.ready || P.seeking) return;
    var cur, target;
    try { cur = av().getCurrentTime(); target = C.seekTarget(cur, deltaMs, av().getDuration()); } catch (e) { return; }
    P.seeking = true; drawBar();
    function done(ok, why) {
      if (!P.seeking) return;
      P.seeking = false;
      if (P.seekTimer) { clearTimeout(P.seekTimer); P.seekTimer = null; }
      if (!ok) { log('seek failed: ' + why); toast('This file cannot be skipped through'); }
      drawBar();
    }
    try {
      av().seekTo(target, function () { done(true); }, function (e) { done(false, errText(e)); });
    } catch (e) { done(false, errText(e)); return; }
    P.seekTimer = setTimeout(function () { done(false, 'no answer'); }, SEEK_TIMEOUT_MS);
  }
  function togglePause() {
    if (!P.ready || P.seeking) return;
    try { if (avState() === 'PLAYING') av().pause(); else av().play(); } catch (e) { log('pause failed ' + errText(e)); }
    drawBar();
  }
  function nextAudio() {
    if (!P.ready || P.seeking || P.tracks.length < 2) return;
    selectAudio((P.track + 1) % P.tracks.length);
    drawBar();
  }

  function showBar() {
    $('bar').className = '';
    if (P.hide) clearTimeout(P.hide);
    P.hide = setTimeout(function () { if (avState() === 'PLAYING') $('bar').className = 'off'; }, BAR_HIDE_MS);
    drawBar();
  }
  function barShown() { return $('bar').className !== 'off'; }
  function drawBar() {
    var cur = 0, dur = 0, html = '';
    if (P.ready) { try { cur = av().getCurrentTime(); dur = av().getDuration(); } catch (e) {} }
    for (var i = 0; i < BUTTONS.length; i++) {
      var b = BUTTONS[i], label = b.label;
      if (b.toggle) label = avState() === 'PAUSED' ? 'Play' : 'Pause';
      if (b.audio) {
        if (P.tracks.length < 2) continue;
        label = 'Audio: ' + C.audioLabel(P.tracks[P.track]);
      }
      html += '<span class="pb' + (i === P.button ? ' sel' : '') + '">' + C.escapeHtml(label) + '</span>';
    }
    $('bar-buttons').innerHTML = html;
    $('bar-time').textContent = (P.seeking ? 'Skipping…  ' : '') + C.fmtTime(cur) + ' / ' + C.fmtTime(dur);
    $('bar-fill').style.width = (dur > 0 ? Math.min(100, cur * 100 / dur) : 0) + '%';
  }
  function barMove(step) {
    var i = P.button;
    do { i = (i + step + BUTTONS.length) % BUTTONS.length; } while (BUTTONS[i].audio && P.tracks.length < 2);
    P.button = i;
  }
  function keyPlayer(k) {
    if (k === KEY.BACK || k === KEY.STOP) { stop(); return; }
    // The media keys act at once, bar or no bar.
    if (k === KEY.PLAYPAUSE || k === KEY.PLAY || k === KEY.PAUSE) { togglePause(); showBar(); return; }
    if (k === KEY.FF) { seek(30000); showBar(); return; }
    if (k === KEY.RW) { seek(-30000); showBar(); return; }
    // A hidden bar takes the first press only to appear.
    if (!barShown()) { showBar(); return; }
    if (k === KEY.LEFT) barMove(-1);
    else if (k === KEY.RIGHT) barMove(1);
    else if (k === KEY.ENTER) {
      var b = BUTTONS[P.button];
      if (b.seek) seek(b.seek); else if (b.toggle) togglePause(); else if (b.audio) nextAudio();
    }
    showBar();
  }

  // ---- Keys and start ----------------------------------------------------

  function exitApp() {
    try { window.tizen.application.getCurrentApplication().exit(); } catch (e) {}
  }
  document.addEventListener('keydown', function (ev) {
    var k = ev.keyCode;
    if (S.view === 'setup') keySetup(k);
    else if (S.view === 'browse') keyBrowse(k);
    else if (S.view === 'detail') keyDetail(k);
    else if (S.view === 'player') keyPlayer(k);
    // Back must never fall through to the TV, which would close the app.
    if (k === KEY.BACK) ev.preventDefault();
  });
  // The TV pauses a hidden app; without this the film keeps its decoder.
  document.addEventListener('visibilitychange', function () {
    if (document.hidden && S.view === 'player') stop();
  });
  try {
    window.tizen.tvinputdevice.registerKeyBatch(['MediaPlay', 'MediaPause', 'MediaPlayPause', 'MediaStop', 'MediaFastForward', 'MediaRewind']);
  } catch (e) { log('media keys not registered: ' + errText(e)); }

  if (S.server && S.token) loadMovies(); else openSetup();
})();

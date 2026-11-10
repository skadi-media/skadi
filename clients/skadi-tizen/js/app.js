// The TV app: the views and the remote-control keys that drive them.
// ES5; see core.js for why. Pure logic lives in core.js, where it is tested.
//
// Views: sign in; the shell (a menu on the left with Home, Movies, TV and
// Search beside it); a movie page; a series page; the player.
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
  var DIR = {}; DIR[KEY.LEFT] = 'left'; DIR[KEY.RIGHT] = 'right'; DIR[KEY.UP] = 'up'; DIR[KEY.DOWN] = 'down';
  var PREF_LANG = 'en';

  var S = {
    view: 'setup',
    server: localStorage.getItem('server') || DEV.server || '',
    token: localStorage.getItem('token') || '',
    movies: [], shows: [], recentMovies: [], recentShows: [],
    // Where started films and episodes were left. Kept on this TV.
    progress: C.readProgress(localStorage.getItem('progress'))
  };
  function storeProgress(list) {
    S.progress = list;
    try { localStorage.setItem('progress', JSON.stringify(list)); } catch (e) { log('progress not stored: ' + errText(e)); }
  }

  function $(id) { return document.getElementById(id); }
  function log(msg) {
    try { console.log(msg); } catch (e) {}
    if (DEV.log) { try { DEV.log(String(msg)); } catch (e2) {} }
  }
  function errText(e) { return (e && (e.name || e.message)) || String(e); }
  window.onerror = function (m, f, l) { log('JS ERROR ' + m + ' @' + l); };
  var esc = C.escapeHtml;

  var toastTimer = null;
  function toast(msg, info) {
    var t = $('toast');
    t.textContent = msg; t.className = info ? 'on info' : 'on';
    if (toastTimer) clearTimeout(toastTimer);
    toastTimer = setTimeout(function () { t.className = ''; }, info ? 3000 : 6000);
    log('toast: ' + msg);
  }
  function untoast() { $('toast').className = ''; }

  var VIEWS = ['setup', 'shell', 'detail', 'series', 'player'];
  function show(view) {
    for (var i = 0; i < VIEWS.length; i++) {
      var el = $(VIEWS[i]), on = VIEWS[i] === view;
      // The player keeps its own extra classes (idle, opts).
      el.className = 'view' + (on ? ' on' : '');
    }
    document.body.className = view === 'player' ? 'playing' : '';
    if (view !== S.view) untoast(); // a message about the last screen does not follow to the next
    S.view = view;
  }

  function api(method, path, body, done) {
    var x = new XMLHttpRequest();
    x.open(method, C.apiUrl(S.server, path), true);
    x.timeout = 30000;
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

  // One poster tile. `sub` is a second caption line. `opts.pct` draws a
  // progress line on the poster; `opts.remove` draws the Remove button over it.
  function tile(item, selected, sub, opts) {
    var img = C.sizedImage(item.poster_url, 'w342'), o = opts || {};
    return '<div class="tile' + (selected ? ' sel' : '') + '"><div class="art">' +
      (img ? '<img src="' + esc(img) + '" alt="">' : '<div class="blank">' + esc(item.title) + '</div>') +
      (o.pct !== undefined ? '<div class="seen"><div style="width:' + Math.round(o.pct) + '%"></div></div>' : '') +
      (o.remove ? '<div class="remove">\u2715 Remove</div>' : '') +
      '</div><div class="name">' + esc(item.title) + (sub ? '<small>' + esc(sub) + '</small>' : '') + '</div></div>';
  }
  function byId(list, id) {
    for (var i = 0; i < list.length; i++) if (list[i].id === id) return list[i];
    return null;
  }

  // ---- Sign in -----------------------------------------------------------

  var FIELDS = ['server', 'user', 'pass', 'go'], field = 0;
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
        loadLibrary();
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
      if (FIELDS[field] === 'go') signIn(); else $('f-' + FIELDS[field]).focus();
    } else if (k === KEY.PLAY || k === KEY.PLAYPAUSE) signIn();
    else if (k === KEY.BACK) exitApp();
    drawSetup();
  }

  // ---- Loading the library -----------------------------------------------

  // Four requests, one after another: the TV is slow enough at parsing that
  // running them together gains nothing.
  function loadLibrary() {
    toast('Loading…', true);
    function fail(status) {
      if (status === 401) { S.token = ''; localStorage.removeItem('token'); toast('Signed out. Sign in again.'); }
      else toast(status === 0 ? 'Cannot reach ' + S.server : 'Could not load the library (' + status + ')');
      openSetup();
    }
    api('GET', '/movies', null, function (st, movies) {
      if (st !== 200 || !movies) return fail(st);
      S.movies = [];
      for (var i = 0; i < movies.length; i++) if (C.playable(movies[i]).length) S.movies.push(movies[i]);
      S.movies.sort(C.byTitle);
      // The summary view: the full list is tens of megabytes of episodes.
      api('GET', '/series?view=summary', null, function (st2, shows) {
        if (st2 !== 200 || !shows) return fail(st2);
        S.shows = [];
        for (var j = 0; j < shows.length; j++) {
          if (!C.importedCount(shows[j])) continue;
          shows[j].episodes = null; // only the count was needed; free the rest
          S.shows.push(shows[j]);
        }
        S.shows.sort(C.byTitle);
        api('GET', '/new-imports?kind=movie&limit=80', null, function (st3, nm) {
          var rows = C.recent((nm && nm.items) || [], 40);
          S.recentMovies = [];
          for (var a = 0; a < rows.length && S.recentMovies.length < 20; a++) {
            var m = byId(S.movies, rows[a].item_id);
            if (m) S.recentMovies.push(m);
          }
          api('GET', '/new-imports?kind=series&limit=200', null, function (st4, ns) {
            S.recentShows = C.recent((ns && ns.items) || [], 20);
            untoast();
            G.movies = { sel: 0, top: 0 }; G.tv = { sel: 0, top: 0 };
            openPane('home'); show('shell'); focusContent();
          });
        });
      });
    });
  }

  // ---- The shell: menu and panes -----------------------------------------

  var RAIL = [['home', 'Home'], ['movies', 'Movies'], ['tv', 'TV'], ['search', 'Search']];
  var shell = { pane: 'home', rail: 0, inRail: false };
  function drawRail() {
    var html = '<div class="brand">Skadi</div>';
    for (var i = 0; i < RAIL.length; i++) {
      html += '<div class="rail-item' + (shell.inRail && i === shell.rail ? ' sel' : RAIL[i][0] === shell.pane ? ' here' : '') + '">' + RAIL[i][1] + '</div>';
    }
    $('rail').innerHTML = html;
  }
  function openPane(name) {
    shell.pane = name;
    for (var i = 0; i < RAIL.length; i++) if (RAIL[i][0] === name) shell.rail = i;
    $('pane-home').className = 'pane' + (name === 'home' ? ' on' : '');
    $('pane-grid').className = 'pane' + (name === 'movies' || name === 'tv' ? ' on' : '');
    $('pane-search').className = 'pane' + (name === 'search' ? ' on' : '');
    drawPane();
  }
  function drawPane() {
    drawRail();
    if (shell.pane === 'home') drawHome();
    else if (shell.pane === 'search') drawSearch();
    else drawGrid();
  }
  function focusRail() { shell.inRail = true; drawPane(); }
  function focusContent() { shell.inRail = false; drawPane(); }
  function keyShell(k) {
    if (shell.inRail) {
      if (k === KEY.UP && shell.rail > 0) { shell.rail--; openPane(RAIL[shell.rail][0]); }
      else if (k === KEY.DOWN && shell.rail < RAIL.length - 1) { shell.rail++; openPane(RAIL[shell.rail][0]); }
      else if (k === KEY.RIGHT || k === KEY.ENTER) focusContent();
      else if (k === KEY.BACK) { if (shell.pane === 'home') exitApp(); else { openPane('home'); } }
      return;
    }
    if (k === KEY.BACK) { focusRail(); return; }
    var leave = shell.pane === 'home' ? keyHome(k) : shell.pane === 'search' ? keySearch(k) : keyGrid(k);
    if (leave === 'rail') focusRail();
  }
  function openItem(kind, item) {
    // A row of the recently-added feed names its series in `item_id`; its own
    // `id` is the id of the feed row.
    if (kind === 'movie') openDetail(item); else openSeries(item.item_id || item.id);
  }

  // ---- Home: rows of recently added --------------------------------------

  var ROW_VISIBLE = 6, HOME_ROWS = 2;
  // `sel` and `left` are kept per row kind, since the rows come and go: the
  // first one is there only while something is part-watched.
  // The selected row is remembered by kind too, or a row appearing above it
  // would move the selection to a different row.
  var home = { kind: '', top: 0, sel: {}, left: {}, removing: false };
  function homeRow(rows) {
    for (var i = 0; i < rows.length; i++) if (rows[i].kind === home.kind) return i;
    return 0;
  }
  function homeRows() {
    var rows = [];
    if (S.progress.length) rows.push({ title: 'Continue watching', kind: 'resume', items: S.progress });
    if (S.recentMovies.length) rows.push({ title: 'Recently added movies', kind: 'movie', items: S.recentMovies });
    if (S.recentShows.length) rows.push({ title: 'Recently added TV', kind: 'series', items: S.recentShows });
    return rows;
  }
  function drawHome() {
    var rows = homeRows(), html = '', row = homeRow(rows);
    home.top = C.rowLeft(row, HOME_ROWS, home.top);
    for (var r = home.top; r < Math.min(rows.length, home.top + HOME_ROWS + 1); r++) {
      var kind = rows[r].kind, it = rows[r].items, active = !shell.inRail && r === row;
      var sel = home.sel[kind] = Math.min(home.sel[kind] || 0, it.length - 1);
      var left = home.left[kind] = C.rowLeft(sel, ROW_VISIBLE, home.left[kind] || 0);
      html += '<div class="row' + (active ? ' sel' : '') + '"><h2>' + rows[r].title +
        (active && kind === 'resume' ? '<small>' + (home.removing ? 'Enter removes it from this row' : '\u25b2 remove from this row') + '</small>' : '') +
        '</h2><div class="tiles">';
      for (var i = left; i < Math.min(it.length, left + ROW_VISIBLE + 1); i++) {
        var on = active && i === sel;
        if (kind === 'resume') {
          html += tile(it[i], on, it[i].kind === 'episode' ? it[i].sub : C.leftLabel(it[i].pos, it[i].dur),
            { pct: it[i].pos * 100 / it[i].dur, remove: on && home.removing });
        } else html += tile(it[i], on, kind === 'series' ? it[i].episode : '');
      }
      html += '</div></div>';
    }
    $('pane-home').innerHTML = html || '<div class="empty">Nothing has been added yet. Movies and TV are in the menu.</div>';
  }
  function keyHome(k) {
    var rows = homeRows();
    if (!rows.length) return k === KEY.LEFT ? 'rail' : null;
    var r = homeRow(rows), kind = rows[r].kind, n = rows[r].items.length;
    if (home.removing) {
      // The Remove button sits over the tile: Enter presses it, anything else leaves it.
      home.removing = false;
      if (k === KEY.ENTER) {
        var gone = rows[r].items[home.sel[kind]];
        storeProgress(C.withoutProgress(S.progress, gone.key));
        log('removed from continue watching: ' + gone.title);
        drawHome();
        return null;
      }
      if (k === KEY.DOWN || k === KEY.UP) { drawHome(); return null; }
    }
    if (k === KEY.LEFT) { if (home.sel[kind] === 0) return 'rail'; home.sel[kind]--; }
    else if (k === KEY.RIGHT) home.sel[kind] = Math.min(n - 1, home.sel[kind] + 1);
    else if (k === KEY.UP) { if (kind === 'resume') home.removing = true; else home.kind = rows[Math.max(0, r - 1)].kind; }
    else if (k === KEY.DOWN) home.kind = rows[Math.min(rows.length - 1, r + 1)].kind;
    else if (k === KEY.ENTER) {
      var item = rows[r].items[home.sel[kind]];
      if (kind === 'resume') resume(item); else openItem(kind, item);
      return null;
    }
    drawHome();
    return null;
  }
  // Pick up a film or an episode where it was left, straight from Home.
  function resume(entry) {
    if (entry.kind === 'movie') {
      var movie = byId(S.movies, entry.id), edition = movie ? byId(C.playable(movie), entry.part) : null;
      if (!edition) { storeProgress(C.withoutProgress(S.progress, entry.key)); toast('That film is no longer in the library'); drawHome(); return; }
      playMovie(movie, edition, entry.pos, 'shell');
      return;
    }
    toast('Loading…', true);
    api('GET', '/series/' + entry.id, null, function (status, series) {
      var ep = status === 200 && series ? byId(series.episodes || [], entry.part) : null;
      if (!ep || !ep.file) {
        if (status === 200 || status === 404) storeProgress(C.withoutProgress(S.progress, entry.key));
        toast(status === 200 || status === 404 ? 'That episode is no longer in the library' : 'Could not load this series (' + status + ')');
        drawHome();
        return;
      }
      untoast();
      playEpisode(series, ep, entry.pos, 'shell');
    });
  }

  // ---- Movies and TV: a grid with a letter strip --------------------------

  var COLS = 6, ROWS = 2;
  var G = { movies: { sel: 0, top: 0 }, tv: { sel: 0, top: 0 }, inLetters: false, letter: '#' };
  function gridItems() { return shell.pane === 'tv' ? S.shows : S.movies; }
  function gridState() { return shell.pane === 'tv' ? G.tv : G.movies; }
  function drawGrid() {
    var items = gridItems(), st = gridState(), html = '', i;
    st.top = C.gridTop(st.sel, COLS, ROWS, st.top);
    var first = st.top * COLS, last = Math.min(items.length, first + COLS * (ROWS + 1));
    for (i = first; i < last; i++) html += tile(items[i], !shell.inRail && !G.inLetters && i === st.sel);
    $('grid').innerHTML = html || '<div class="empty">Nothing here yet.</div>';
    var cur = items[st.sel];
    $('grid-name').textContent = shell.pane === 'tv' ? 'TV' : 'Movies';
    $('grid-count').textContent = items.length ? (st.sel + 1) + ' of ' + items.length : '';
    $('grid-title').textContent = cur ? cur.title + (cur.year ? ' (' + cur.year + ')' : '') : '';
    var index = C.letterIndex(items), here = cur ? C.letterOf(cur) : '';
    html = '';
    for (i = 0; i < C.LETTERS.length; i++) {
      var l = C.LETTERS[i];
      html += '<div class="letter' + (l in index ? ' has' : '') +
        (G.inLetters && l === G.letter ? ' sel' : l === here ? ' here' : '') + '">' + l + '</div>';
    }
    $('letters').innerHTML = html;
  }
  function keyGrid(k) {
    var items = gridItems(), st = gridState();
    if (!items.length) return k === KEY.LEFT ? 'rail' : null;
    if (G.inLetters) {
      var index = C.letterIndex(items);
      if (k === KEY.UP || k === KEY.DOWN) {
        G.letter = C.nextLetter(index, G.letter, k === KEY.UP ? -1 : 1);
        st.sel = index[G.letter]; st.top = Math.floor(st.sel / COLS);
      } else if (k === KEY.LEFT || k === KEY.ENTER) G.inLetters = false;
      drawGrid();
      return null;
    }
    if (k === KEY.LEFT && st.sel % COLS === 0) return 'rail';
    if (k === KEY.RIGHT && (st.sel % COLS === COLS - 1 || st.sel === items.length - 1)) {
      G.inLetters = true; G.letter = C.letterOf(items[st.sel]);
    } else if (DIR[k]) st.sel = C.gridMove(st.sel, DIR[k], COLS, items.length);
    else if (k === KEY.FF) st.sel = Math.min(items.length - 1, st.sel + COLS * ROWS * 3);
    else if (k === KEY.RW) st.sel = Math.max(0, st.sel - COLS * ROWS * 3);
    else if (k === KEY.ENTER) { openItem(shell.pane === 'tv' ? 'series' : 'movie', items[st.sel]); return null; }
    drawGrid();
    return null;
  }

  // ---- Search: a letter grid, results as you type --------------------------

  var KEYS = 'ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789'.split(''), KCOLS = 6, RCOLS = 4;
  var WIDE = ['Space', 'Delete', 'Clear'];
  var Q = { text: '', key: 0, results: [], sel: 0, top: 0, inResults: false };
  function runSearch() {
    var all = [], i;
    for (i = 0; i < S.movies.length; i++) all.push(S.movies[i]);
    for (i = 0; i < S.shows.length; i++) all.push(S.shows[i]);
    Q.results = C.search(all, Q.text, 60);
    Q.sel = 0; Q.top = 0;
  }
  function isShow(item) { return S.shows.indexOf(item) >= 0; }
  function drawSearch() {
    var html = '', i, keysOn = !shell.inRail && !Q.inResults;
    $('search-text').innerHTML = Q.text ? esc(Q.text) + '<span class="ph">|</span>' : '<span class="ph">Search movies and TV</span>';
    for (i = 0; i < KEYS.length; i++) html += '<div class="key' + (keysOn && i === Q.key ? ' sel' : '') + '">' + KEYS[i] + '</div>';
    for (i = 0; i < WIDE.length; i++) html += '<div class="key wide' + (keysOn && Q.key === KEYS.length + i ? ' sel' : '') + '">' + WIDE[i] + '</div>';
    $('search-keys').innerHTML = html;
    Q.top = C.gridTop(Q.sel, RCOLS, 2, Q.top);
    html = '';
    for (i = Q.top * RCOLS; i < Math.min(Q.results.length, Q.top * RCOLS + RCOLS * 3); i++) {
      html += tile(Q.results[i], !shell.inRail && Q.inResults && i === Q.sel, isShow(Q.results[i]) ? 'TV' : '');
    }
    $('search-results').innerHTML = html;
    $('search-head').textContent = !Q.text ? 'Type a few letters of a title' :
      Q.results.length ? Q.results.length + (Q.results.length === 60 ? '+' : '') + ' found' : 'Nothing found for “' + Q.text + '”';
  }
  function typeKey() {
    if (Q.key < KEYS.length) Q.text += KEYS[Q.key];
    else if (Q.key === KEYS.length) { if (Q.text && Q.text.charAt(Q.text.length - 1) !== ' ') Q.text += ' '; }
    else if (Q.key === KEYS.length + 1) Q.text = Q.text.slice(0, -1);
    else Q.text = '';
    if (Q.text.length > 30) Q.text = Q.text.slice(0, 30);
    runSearch();
  }
  function keySearch(k) {
    if (Q.inResults) {
      if (k === KEY.LEFT && Q.sel % RCOLS === 0) Q.inResults = false;
      else if (DIR[k]) Q.sel = C.gridMove(Q.sel, DIR[k], RCOLS, Q.results.length);
      else if (k === KEY.ENTER && Q.results[Q.sel]) { openItem(isShow(Q.results[Q.sel]) ? 'series' : 'movie', Q.results[Q.sel]); return null; }
      drawSearch();
      return null;
    }
    var n = KEYS.length, wide = Q.key >= n;
    if (k === KEY.ENTER) typeKey();
    else if (k === KEY.LEFT) {
      if (wide ? Q.key === n : Q.key % KCOLS === 0) return 'rail';
      Q.key--;
    } else if (k === KEY.RIGHT) {
      if (wide ? Q.key === n + WIDE.length - 1 : Q.key % KCOLS === KCOLS - 1) { if (Q.results.length) Q.inResults = true; }
      else Q.key++;
    } else if (k === KEY.UP) {
      if (wide) Q.key = n - KCOLS + (Q.key - n) * 2;
      else if (Q.key >= KCOLS) Q.key -= KCOLS;
    } else if (k === KEY.DOWN) {
      if (!wide) Q.key = Q.key + KCOLS < n ? Q.key + KCOLS : n + Math.floor((Q.key % KCOLS) / 2);
    }
    drawSearch();
    return null;
  }

  // ---- Movie page --------------------------------------------------------

  var D = { movie: null, actions: [], action: 0 };
  function setBackdrop(id, url) {
    var bg = C.sizedImage(url, 'w1280');
    $(id).style.backgroundImage = bg ? 'url("' + bg.replace(/"/g, '%22') + '")' : 'none';
  }
  function metaLine(item, extra) {
    var meta = [];
    if (item.year) meta.push(item.year);
    if (item.content_rating) meta.push(item.content_rating);
    if (extra) meta.push(extra);
    if (item.genres && item.genres.length) meta.push(item.genres.slice(0, 3).join(', '));
    return meta.join('  ·  ');
  }
  // One Play button per file; a part-watched file gets Resume first.
  function movieActions(movie) {
    var out = [], eds = C.playable(movie);
    for (var i = 0; i < eds.length; i++) {
      var p = C.findProgress(S.progress, C.progressKey('movie', movie.id), eds[i].id);
      var what = C.mediaSummary(eds[i]) || 'File ' + (i + 1);
      if (p) out.push({ label: '\u25b6 Resume', small: 'from ' + C.fmtTime(p.pos) + '  \u00b7  ' + what, edition: eds[i], pos: p.pos });
      out.push({ label: p ? 'From the start' : '\u25b6 Play', small: what, edition: eds[i], pos: 0 });
    }
    return out;
  }
  function openDetail(movie) {
    D.movie = movie; D.actions = movieActions(movie); D.action = 0;
    $('detail-title').textContent = movie.title;
    $('detail-meta').textContent = metaLine(movie, C.fmtRuntime(movie.runtime_minutes));
    $('detail-overview').textContent = movie.overview || '';
    $('detail-poster').src = C.sizedImage(movie.poster_url, 'w500');
    setBackdrop('detail-backdrop', movie.backdrop_url);
    show('detail'); drawActions();
  }
  function drawActions() {
    var html = '';
    for (var i = 0; i < D.actions.length; i++) {
      html += '<span class="action' + (i === D.action ? ' sel' : '') + '">' + D.actions[i].label + '<small>' + esc(D.actions[i].small) + '</small></span>';
    }
    $('detail-actions').innerHTML = html;
  }
  function keyDetail(k) {
    if (k === KEY.LEFT) D.action = Math.max(0, D.action - 1);
    else if (k === KEY.RIGHT) D.action = Math.min(D.actions.length - 1, D.action + 1);
    else if (k === KEY.ENTER || k === KEY.PLAY || k === KEY.PLAYPAUSE) { playMovie(D.movie, D.actions[D.action].edition, D.actions[D.action].pos, 'detail'); return; }
    else if (k === KEY.BACK) { show('shell'); drawPane(); return; }
    drawActions();
  }

  // ---- Series page -------------------------------------------------------

  var EP_VISIBLE = 7;
  var R = { series: null, seasons: [], si: 0, eps: [], sel: 0, top: 0, inSeasons: false };
  function openSeries(id) {
    toast('Loading…', true);
    // The whole series with its episodes: the list had only a summary.
    api('GET', '/series/' + id, null, function (status, series) {
      if (status !== 200 || !series) { toast('Could not load this series (' + status + ')'); return; }
      untoast();
      R.series = series; R.seasons = C.seasons(series); R.si = 0; R.inSeasons = false;
      $('series-title').textContent = series.title;
      $('series-meta').textContent = metaLine(series, series.network);
      $('series-overview').textContent = series.overview || '';
      $('series-poster').src = C.sizedImage(series.poster_url, 'w500');
      setBackdrop('series-backdrop', series.backdrop_url);
      pickSeason(0);
      // Open on the episode that was being watched, if there is one.
      var was = C.findProgress(S.progress, C.progressKey('episode', series.id)), wasEp = was ? byId(series.episodes || [], was.part) : null;
      if (wasEp && R.seasons.indexOf(wasEp.season) >= 0) {
        pickSeason(R.seasons.indexOf(wasEp.season));
        for (var i = 0; i < R.eps.length; i++) if (R.eps[i].id === wasEp.id) R.sel = i;
      }
      show('series'); drawSeries();
    });
  }
  function pickSeason(si) {
    R.si = si; R.eps = R.seasons.length ? C.episodesOf(R.series, R.seasons[si]) : [];
    R.sel = 0; R.top = 0;
    // Land on the first episode there is a file for.
    for (var i = 0; i < R.eps.length; i++) if (R.eps[i].file) { R.sel = i; break; }
  }
  function epProgress(ep) { return C.findProgress(S.progress, C.progressKey('episode', R.series.id), ep.id); }
  function seasonName(n) { return n === 0 ? 'Specials' : 'Season ' + n; }
  function drawSeries() {
    var html = '', i;
    for (i = 0; i < R.seasons.length; i++) {
      html += '<span class="season' + (i === R.si ? (R.inSeasons ? ' sel' : ' here') : '') + '">' + seasonName(R.seasons[i]) + '</span>';
    }
    $('series-seasons').innerHTML = html;
    R.top = C.rowLeft(R.sel, EP_VISIBLE, R.top);
    html = '';
    for (i = R.top; i < Math.min(R.eps.length, R.top + EP_VISIBLE); i++) {
      var ep = R.eps[i], info = ep.file ? C.mediaSummary(ep) : 'Not in the library';
      var mins = ep.media_info && ep.media_info.duration_secs ? Math.round(ep.media_info.duration_secs / 60) + ' min' : '';
      var left = epProgress(ep);
      if (left) mins = 'Resume at ' + C.fmtTime(left.pos);
      html += '<div class="ep' + (ep.file ? '' : ' none') + (!R.inSeasons && i === R.sel ? ' sel' : '') + '">' +
        '<span class="info">' + esc([mins, info].filter(Boolean).join('  ·  ')) + '</span>' +
        '<span class="num">' + ep.number + '</span>' + esc(ep.title || 'Episode ' + ep.number) + '</div>';
    }
    $('series-episodes').innerHTML = html || '<div class="empty">No episodes in the library.</div>';
  }
  function keySeries(k) {
    if (k === KEY.BACK) { show('shell'); drawPane(); return; }
    if (R.inSeasons) {
      if (k === KEY.LEFT && R.si > 0) pickSeason(R.si - 1);
      else if (k === KEY.RIGHT && R.si < R.seasons.length - 1) pickSeason(R.si + 1);
      else if (k === KEY.DOWN || k === KEY.ENTER) R.inSeasons = false;
    } else if (k === KEY.UP) { if (R.sel === 0) R.inSeasons = true; else R.sel--; }
    else if (k === KEY.DOWN) R.sel = Math.min(R.eps.length - 1, R.sel + 1);
    else if (k === KEY.LEFT && R.si > 0) pickSeason(R.si - 1);
    else if (k === KEY.RIGHT && R.si < R.seasons.length - 1) pickSeason(R.si + 1);
    else if (k === KEY.ENTER || k === KEY.PLAY || k === KEY.PLAYPAUSE) {
      var ep = R.eps[R.sel];
      if (ep && ep.file) { var was = epProgress(ep); playEpisode(R.series, ep, was ? was.pos : 0, 'series'); return; }
      if (ep) toast('This episode is not in the library', true);
    }
    drawSeries();
  }

  // ---- Player ------------------------------------------------------------
  //
  // The usual TV player: a title at the top and one scrub line at the bottom.
  // Left and Right skip, and a held key skips faster. Enter pauses. Down
  // reaches the options (audio track). Everything hides after a few seconds.

  var HIDE_MS = 4000, COMMIT_MS = 600, SEEK_TIMEOUT_MS = 10000;
  var P = {
    ready: false, from: 'detail', series: null, episode: null, ctx: null, ticks: 0,
    tracks: [], track: -1, inOpts: false,
    seeking: false, seekTo: 0, pend: null, commit: null, seekTimer: null,
    tick: null, hide: null, flash: null
  };
  function av() { return window.webapis && window.webapis.avplay; }
  function avState() { try { return av().getState(); } catch (e) { return 'NONE'; } }

  function playMovie(movie, edition, pos, from) {
    var info = edition.media_info || {};
    var url = DEV.videoUrl ? DEV.videoUrl(S.server, movie.id, edition.id, S.token, info.container || '')
      : C.videoUrl(S.server, movie.id, edition.id, S.token);
    P.series = null; P.episode = null;
    P.ctx = { key: C.progressKey('movie', movie.id), kind: 'movie', id: movie.id, part: edition.id, title: movie.title, sub: '', poster_url: movie.poster_url };
    start(url, from, movie.title, metaLine(movie, ''), pos);
  }
  function playEpisode(series, ep, pos, from) {
    var info = ep.media_info || {};
    var url = DEV.episodeUrl ? DEV.episodeUrl(S.server, series.id, ep.id, S.token, info.container || '')
      : C.episodeVideoUrl(S.server, series.id, ep.id, S.token);
    P.series = series; P.episode = ep;
    P.ctx = { key: C.progressKey('episode', series.id), kind: 'episode', id: series.id, part: ep.id, title: series.title, sub: C.episodeCode(ep), poster_url: series.poster_url };
    start(url, from, series.title, C.episodeCode(ep) + (ep.title ? '  \u00b7  ' + ep.title : ''), pos);
  }
  function start(url, from, title, sub, pos) {
    if (!av()) { toast('This device has no Samsung player'); return; }
    clearPlayerTimers();
    P.ready = false; P.from = from; P.tracks = []; P.track = -1; P.inOpts = false;
    P.seeking = false; P.pend = null; P.ticks = 0;
    $('pt-title').textContent = title; $('pt-sub').textContent = sub || '';
    show('player'); flash('Loading…', true); wake();
    log('play ' + title + ' ' + (sub || '') + (pos ? ' from ' + C.fmtTime(pos) : ''));
    function go() {
      if (S.view !== 'player' || P.ready) return;
      P.ready = true;
      unflash();
      try { av().play(); } catch (e) { log('play failed ' + errText(e)); }
      chooseAudio();
      P.tick = setInterval(function () {
        // Every ten seconds, so a power cut loses little.
        if (++P.ticks % 10 === 0) saveNow();
        drawPlayer();
      }, 1000);
      wake();
    }
    try {
      try { av().close(); } catch (e0) {}
      av().open(url);
      av().setDisplayRect(0, 0, 1920, 1080);
      av().setListener({
        onbufferingstart: function () { if (P.ready) flash('Loading…', true); },
        onbufferingcomplete: function () { unflash(); },
        onstreamcompleted: function () { log('stream completed'); ended(); },
        onerror: function (e) { log('player error ' + e); toast('The TV could not play this file (' + e + ')'); stop(); },
        onevent: function () {}, oncurrentplaytime: function () {}, onsubtitlechange: function () {}
      });
      av().prepareAsync(function () {
        if (S.view !== 'player') return; // Back was pressed while it loaded
        if (!pos) { go(); return; }
        // Resume: move first, then play, so the start of the film never
        // flashes by. The player allows no other call until the seek answers;
        // if it never does, play from the start instead of hanging.
        var fallback = setTimeout(function () { log('resume seek: no answer'); go(); }, 8000);
        function then(why) { clearTimeout(fallback); if (why) log('resume seek failed: ' + why); go(); }
        try { av().seekTo(pos, function () { then(); }, function (e) { then(errText(e)); }); } catch (e) { then(errText(e)); }
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
  // Remember where this is, for Continue watching.
  function saveNow() {
    if (!P.ready || !P.ctx) return;
    var cur, dur;
    try { cur = av().getCurrentTime(); dur = av().getDuration(); } catch (e) { return; }
    if (!dur) return;
    var c = P.ctx;
    storeProgress(C.saveProgress(S.progress, {
      key: c.key, kind: c.kind, id: c.id, part: c.part, title: c.title, sub: c.sub, poster_url: c.poster_url,
      pos: P.pend ? P.pend.target : P.seeking ? P.seekTo : cur, dur: dur, at: Date.now()
    }));
  }

  // The first track is not always the right one. Pick by language and by
  // what the TV decodes.
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

  function clearPlayerTimers() {
    var names = ['tick', 'hide', 'commit', 'seekTimer', 'flash'];
    for (var i = 0; i < names.length; i++) if (P[names[i]]) { clearTimeout(P[names[i]]); clearInterval(P[names[i]]); P[names[i]] = null; }
  }
  function stop() {
    saveNow();
    clearPlayerTimers();
    P.ready = false; P.seeking = false; P.pend = null;
    try { av().stop(); } catch (e) {}
    try { av().close(); } catch (e2) {}
    unflash();
    if (S.view !== 'player') return;
    if (P.from === 'series') { show('series'); drawSeries(); }
    else if (P.from === 'shell') { show('shell'); drawPane(); }
    else { D.actions = movieActions(D.movie); D.action = 0; show('detail'); drawActions(); }
  }
  // The end of the file. A series goes on to its next episode.
  function ended() {
    // Finished: it leaves Continue watching. A next episode comes back into
    // the row by itself once it has played for a while.
    if (P.ctx) storeProgress(C.withoutProgress(S.progress, P.ctx.key));
    var next = P.series && P.episode ? C.nextEpisode(P.series, P.episode) : null;
    P.ready = false; // nothing more to save for the file that ended
    if (!next) { stop(); return; }
    var series = P.series, from = P.from;
    // Keep the series page on the episode now playing, for when Back is pressed.
    if (from === 'series' && R.series && R.series.id === series.id) {
      var si = R.seasons.indexOf(next.season);
      if (si >= 0) { pickSeason(si); for (var i = 0; i < R.eps.length; i++) if (R.eps[i].id === next.id) R.sel = i; }
    }
    clearPlayerTimers();
    try { av().stop(); } catch (e) {}
    playEpisode(series, next, 0, from);
  }

  // Skipping. Each press moves a target, shown on the scrub line at once; the
  // player is asked to go there only when the presses stop. Samsung's player
  // takes one seek at a time and refuses every call made while one runs, so a
  // seek per press would drop most of them.
  function skip(dir) {
    if (!P.ready) return;
    var now = Date.now(), cur, dur;
    try { cur = av().getCurrentTime(); dur = av().getDuration(); } catch (e) { return; }
    if (!P.pend) P.pend = { target: P.seeking ? P.seekTo : cur, repeats: 0, at: 0 };
    P.pend.repeats = now - P.pend.at < 500 ? P.pend.repeats + 1 : 0;
    P.pend.at = now;
    P.pend.target = C.seekTarget(P.pend.target, dir * C.skipStep(P.pend.repeats), dur);
    if (P.commit) clearTimeout(P.commit);
    P.commit = setTimeout(commitSkip, COMMIT_MS);
    wake();
  }
  function commitSkip() {
    P.commit = null;
    if (!P.pend || !P.ready) return;
    if (P.seeking) { P.commit = setTimeout(commitSkip, 200); return; } // wait for the one in flight
    var target = P.pend.target;
    P.pend = null; P.seeking = true; P.seekTo = target;
    function done(ok, why) {
      if (!P.seeking) return;
      P.seeking = false;
      if (P.seekTimer) { clearTimeout(P.seekTimer); P.seekTimer = null; }
      if (!ok) { log('seek failed: ' + why); toast('This file cannot be skipped through'); }
      wake();
    }
    try {
      av().seekTo(target, function () { done(true); }, function (e) { done(false, errText(e)); });
    } catch (e) { done(false, errText(e)); return; }
    // Some files never answer a seek. Free the controls anyway.
    P.seekTimer = setTimeout(function () { done(false, 'no answer'); }, SEEK_TIMEOUT_MS);
    drawPlayer();
  }
  function togglePause() {
    if (!P.ready || P.seeking) return;
    try {
      if (avState() === 'PLAYING') { av().pause(); flash('❚❚'); } else { av().play(); flash('▶'); }
    } catch (e) { log('pause failed ' + errText(e)); }
    wake();
  }
  function nextAudio() {
    if (!P.ready || P.seeking || P.tracks.length < 2) return;
    selectAudio((P.track + 1) % P.tracks.length);
    wake();
  }

  // The round sign in the middle: pause and play for a moment, "Loading…" until told otherwise.
  function flash(text, stay) {
    var el = $('player-state');
    el.textContent = text; el.className = 'on' + (text.length > 2 ? ' text' : '');
    if (P.flash) clearTimeout(P.flash);
    P.flash = stay ? null : setTimeout(unflash, 900);
  }
  function unflash() { $('player-state').className = ''; }

  // Show the controls and start the clock that hides them again.
  function wake() {
    if (P.hide) clearTimeout(P.hide);
    P.hide = setTimeout(function () {
      // Stay up while paused or mid-skip: that is when they are being read.
      if (avState() === 'PLAYING' && !P.pend && !P.seeking) { P.inOpts = false; setPlayerClass(true); } else wake();
    }, HIDE_MS);
    setPlayerClass(false);
    drawPlayer();
  }
  function setPlayerClass(idle) {
    if (S.view !== 'player') return;
    $('player').className = 'view on' + (idle ? ' idle' : '') + (P.inOpts ? ' opts' : '');
  }
  function controlsShown() { return $('player').className.indexOf('idle') < 0; }
  function drawPlayer() {
    if (S.view !== 'player') return;
    var cur = 0, dur = 0;
    if (P.ready) { try { cur = av().getCurrentTime(); dur = av().getDuration(); } catch (e) {} }
    var at = P.pend ? P.pend.target : P.seeking ? P.seekTo : cur;
    var pct = dur > 0 ? Math.min(100, at * 100 / dur) : 0;
    $('pb-cur').textContent = C.fmtTime(at);
    $('pb-total').textContent = dur ? '−' + C.fmtTime(Math.max(0, dur - at)) : '';
    $('pb-fill').style.width = pct + '%'; $('pb-knob').style.left = pct + '%';
    var html = '';
    if (P.tracks.length > 1) html += '<span class="opt' + (P.inOpts ? ' sel' : '') + '">Audio: ' + esc(C.audioLabel(P.tracks[P.track])) + '</span>';
    html += '<span class="opt-hint">' + (P.inOpts ? 'Enter changes the audio track' :
      '◀ ▶ skip  ·  Enter pause' + (P.tracks.length > 1 ? '  ·  ▼ audio' : '')) + '</span>';
    $('pb-opts').innerHTML = html;
  }
  function keyPlayer(k) {
    if (k === KEY.BACK || k === KEY.STOP) { stop(); return; }
    if (k === KEY.PLAYPAUSE || k === KEY.PLAY || k === KEY.PAUSE) { togglePause(); return; }
    if (k === KEY.FF) { skip(1); return; }
    if (k === KEY.RW) { skip(-1); return; }
    if (P.inOpts && controlsShown()) {
      if (k === KEY.ENTER) nextAudio();
      else if (k === KEY.UP) P.inOpts = false;
      wake();
      return;
    }
    if (k === KEY.LEFT) skip(-1);
    else if (k === KEY.RIGHT) skip(1);
    else if (k === KEY.ENTER) togglePause();
    else if (k === KEY.DOWN) { if (controlsShown() && P.tracks.length > 1) P.inOpts = true; wake(); }
    else wake();
  }

  // ---- Keys and start ----------------------------------------------------

  function exitApp() {
    try { window.tizen.application.getCurrentApplication().exit(); } catch (e) {}
  }
  document.addEventListener('keydown', function (ev) {
    var k = ev.keyCode;
    if (S.view === 'setup') keySetup(k);
    else if (S.view === 'shell') keyShell(k);
    else if (S.view === 'detail') keyDetail(k);
    else if (S.view === 'series') keySeries(k);
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

  if (S.server && S.token) loadLibrary(); else openSetup();
})();

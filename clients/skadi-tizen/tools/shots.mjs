// Screenshot harness: run the TV app in desktop Chrome against a fake server
// and a fake Samsung player, press remote keys, and save a picture of each
// view to build/shots/.
//
//   npm install        (once; fetches playwright-core)
//   npm run shots
//
// It is for layout and flow. It is NOT the TV: desktop Chrome is years newer
// than the TV's Chromium M47, so a page that looks right here can still break
// there (CSS grid, custom properties and modern JavaScript all work here).
// The library is made up, so the pictures hold no real titles.
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright-core';

const APP = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const OUT = path.join(APP, 'build', 'shots');
const KEY = { LEFT: 37, UP: 38, RIGHT: 39, DOWN: 40, ENTER: 13, BACK: 10009 };

const WORDS = ['Harbour', 'Lantern', 'Orchard', 'Signal', 'Meridian', 'Paper', 'Winter', 'Copper', 'Atlas', 'Tide',
  'Quiet', 'Glass', 'Northern', 'Hollow', 'Ember', 'Marble', 'Second', 'Violet', 'Salt', 'Thunder'];
const NOUNS = ['Road', 'Garden', 'Letters', 'Machine', 'Station', 'Summer', 'Kingdom', 'Divide', 'Protocol', 'Season'];
const HUES = [205, 20, 140, 280, 45, 330, 170, 0, 250, 95];

function movies() {
  const out = [];
  for (let i = 0; i < 60; i++) {
    const title = i === 4 ? 'The Extraordinarily Long Title of a Film That Will Not Fit on One Line'
      : `${i % 3 === 0 ? 'The ' : ''}${WORDS[i % WORDS.length]} ${NOUNS[(i * 7) % NOUNS.length]}`;
    const fourK = i % 4 === 1;
    const editions = [{
      id: `e${i}`, file: { path: `/library/${i}.mkv` },
      media_info: {
        container: 'mkv', duration_secs: 6000 + i * 37,
        video: { width: fourK ? 3840 : 1920, height: fourK ? 2160 : 1080, codec: fourK ? 'hevc' : 'h264', dynamic_range: fourK ? 'hdr10' : null },
        audio: { codec: i % 5 === 0 ? 'dts' : 'eac3', channels: 6 },
      },
    }];
    if (i === 0) editions.push({ id: 'e0b', file: { path: '/library/0b.mp4' }, media_info: { container: 'mp4', video: { width: 1920, height: 1080, codec: 'h264' }, audio: { codec: 'aac', channels: 2 } } });
    out.push({
      id: `m${i}`, title, year: 1980 + (i * 3) % 45, runtime_minutes: 88 + (i * 5) % 70,
      content_rating: ['PG', 'PG-13', 'R', 'G'][i % 4], genres: ['Drama', 'Adventure', 'Mystery'].slice(0, 1 + (i % 3)),
      overview: 'A made-up film for the screenshot harness. '.repeat(2 + (i % 5)) + 'It stands in for a real description, which runs to a few sentences and has to wrap inside the page without pushing the buttons off the screen.',
      // i === 7 has no poster, to show the fallback tile.
      poster_url: i === 7 ? null : `/fixture/poster/${i}.svg`, backdrop_url: `/fixture/backdrop/${i}.svg`,
      editions,
    });
  }
  // One movie with no file: the app must leave it out.
  out.push({ id: 'missing', title: 'Not Downloaded Yet', year: 2026, editions: [{ id: 'x' }] });
  return out;
}

function shows() {
  const out = [];
  for (let i = 0; i < 30; i++) {
    const episodes = [];
    const seasonCount = 1 + (i % 4);
    for (let s = 1; s <= seasonCount; s++) {
      for (let n = 1; n <= 10; n++) {
        // Season 1 episode 4 was never downloaded: the page must show the gap.
        const have = !(s === 1 && n === 4);
        episodes.push({
          id: `s${i}e${s}x${n}`, season: s, number: n, title: `${WORDS[(n + s) % WORDS.length]} ${NOUNS[(n * 3) % NOUNS.length]}`,
          status: have ? { Imported: {} } : 'Missing', file: have ? { path: `/tv/${i}/${s}/${n}.mkv` } : null,
          media_info: have ? { container: 'mkv', duration_secs: 2580 + n * 11, video: { width: 1920, height: 1080, codec: 'hevc' }, audio: { codec: 'eac3', channels: 6 } } : null,
        });
      }
    }
    if (i === 0) episodes.push({ id: 's0sp', season: 0, number: 1, title: 'A Special', status: { Imported: {} }, file: { path: '/tv/0/0/1.mkv' }, media_info: null });
    out.push({
      id: `show${i}`, title: `${WORDS[(i * 3) % WORDS.length]} ${NOUNS[(i + 4) % NOUNS.length]}${i % 5 === 0 ? ' Files' : ''}`,
      sort_title: null, year: 1995 + i, network: ['Channel One', 'Northstar', 'Meridian TV'][i % 3], content_rating: ['TV-14', 'TV-PG', 'TV-MA'][i % 3],
      genres: ['Drama', 'Mystery'], overview: 'A made-up series for the screenshot harness, with a description long enough to need clipping when it runs past the two lines the page gives it. '.repeat(2),
      poster_url: `/fixture/poster/${100 + i}.svg`, backdrop_url: `/fixture/backdrop/${100 + i}.svg`, episodes,
    });
  }
  // A series nobody has downloaded anything of: the app must leave it out.
  out.push({ id: 'empty-show', title: 'Nothing Downloaded', year: 2026, episodes: [{ id: 'x', season: 1, number: 1, status: 'Missing', file: null }] });
  return out;
}
const withSort = (list) => list.map((x) => ({ ...x, sort_title: x.title.toLowerCase().replace(/^the /, '') }));
function newImports(kind) {
  const items = [];
  if (kind === 'movie') {
    // m9 arrives twice (an upgrade first): it must show once.
    for (const i of [9, 9, 31, 2, 44, 17, 58, 23, 5, 40]) items.push({ id: `h${items.length}`, kind: 'movie', item_id: `m${i}`, upgrade: items.length === 0, title: 'x', poster_url: null });
  } else {
    // A whole season of show3 landed at once: one tile, not ten.
    for (let n = 10; n >= 1; n--) items.push({ id: `t${n}`, kind: 'series', item_id: 'show3', title: shows()[3].title, episode: `S01E${String(n).padStart(2, '0')}`, poster_url: '/fixture/poster/103.svg' });
    for (const i of [12, 7, 21, 1, 28, 15, 9]) items.push({ id: `u${i}`, kind: 'series', item_id: `show${i}`, title: shows()[i].title, episode: 'S02E05', poster_url: `/fixture/poster/${100 + i}.svg` });
  }
  return { items, next_cursor: null };
}

function svg(w, h, hue, label) {
  return `<svg xmlns="http://www.w3.org/2000/svg" width="${w}" height="${h}">` +
    `<defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="hsl(${hue},55%,42%)"/><stop offset="1" stop-color="hsl(${(hue + 50) % 360},60%,18%)"/></linearGradient></defs>` +
    `<rect width="100%" height="100%" fill="url(#g)"/><text x="50%" y="52%" fill="rgba(255,255,255,.75)" font-family="Arial" font-size="${Math.round(w / 6)}" text-anchor="middle">${label}</text></svg>`;
}

// What `webapis.avplay` and `tizen` are on the TV, as far as the app uses them.
const FAKE_TV = `
window.tizen = { tvinputdevice: { registerKeyBatch: function () {} }, application: { getCurrentApplication: function () { return { exit: function () { window.__exited = true; } }; } } };
(function () {
  var state = 'NONE', pos = 0, timer = null, dur = 6000000, listener = {};
  function tick() { if (state === 'PLAYING') pos = Math.min(dur, pos + 250); }
  window.webapis = { avplay: {
    open: function (url) { window.__opened = url; state = 'IDLE'; pos = 0; },
    close: function () { state = 'NONE'; if (timer) clearInterval(timer); timer = null; },
    setDisplayRect: function () {}, setListener: function (l) { listener = l; window.__endStream = function () { l.onstreamcompleted(); }; },
    prepareAsync: function (ok) { setTimeout(function () { state = 'READY'; ok(); }, 50); },
    play: function () { state = 'PLAYING'; if (!timer) timer = setInterval(tick, 250); },
    pause: function () { state = 'PAUSED'; }, stop: function () { state = 'IDLE'; },
    getState: function () { return state; }, getCurrentTime: function () { return pos; }, getDuration: function () { return dur; },
    seekTo: function (ms, ok) { setTimeout(function () { pos = ms; ok(); }, 80); },
    setSelectTrack: function (type, index) { window.__audio = index; },
    getTotalTrackInfo: function () { return [
      { index: 0, type: 'VIDEO', extra_info: '{"fourCC":"h265"}' },
      { index: 1, type: 'AUDIO', extra_info: '{"language":"ru","channels":"6","fourCC":"ac3"}' },
      { index: 2, type: 'AUDIO', extra_info: '{"language":"en","channels":"6","fourCC":"dts"}' },
      { index: 3, type: 'TEXT', extra_info: '{}' } ]; }
  } };
})();
// On the TV a film shows through wherever the page is not opaque. Stripes
// stand in for the film, so a view that forgot its background is obvious.
// Desktop Chrome draws a "plugin not supported" box for the player object, so
// a plain element takes its place.
document.addEventListener('DOMContentLoaded', function () {
  var film = document.createElement('div');
  film.id = 'av';
  film.style.background = 'repeating-linear-gradient(45deg,#3a2a55 0,#3a2a55 80px,#6b4a2a 80px,#6b4a2a 160px)';
  var plugin = document.getElementById('av');
  plugin.parentNode.replaceChild(film, plugin);
});
`;

const TYPES = { '.html': 'text/html', '.css': 'text/css', '.js': 'application/javascript', '.svg': 'image/svg+xml' };
const server = http.createServer((req, res) => {
  const url = new URL(req.url, 'http://x');
  const send = (code, type, body) => { res.writeHead(code, { 'Content-Type': type }); res.end(body); };
  if (url.pathname.endsWith('/webapis/webapis.js')) return send(200, TYPES['.js'], FAKE_TV);
  if (url.pathname === '/js/dev.local.js') return send(200, TYPES['.js'], ''); // a developer's own file must not change the pictures
  if (url.pathname === '/api/v1/auth/login') {
    let body = '';
    req.on('data', (c) => { body += c; });
    req.on('end', () => {
      const ok = JSON.parse(body || '{}').password === 'right';
      send(ok ? 200 : 401, 'application/json', JSON.stringify(ok ? { token: 'harness-token', member: {} } : { error: 'bad credentials' }));
    });
    return;
  }
  if (url.pathname === '/api/v1/movies') return send(200, 'application/json', JSON.stringify(withSort(movies())));
  if (url.pathname === '/api/v1/series') {
    // The app must ask for the summary view; the full list is far too big for the TV.
    if (url.searchParams.get('view') !== 'summary') return send(400, 'application/json', '{"error":"harness: the app asked for the full series list"}');
    return send(200, 'application/json', JSON.stringify(withSort(shows()).map((s) => ({ ...s, episodes: s.episodes.map((e) => ({ id: e.id, season: e.season, number: e.number, status: e.file ? 'Imported' : 'Missing' })) }))));
  }
  let sm = url.pathname.match(/^\/api\/v1\/series\/([^/]+)$/);
  if (sm) { const one = shows().find((s) => s.id === sm[1]); return one ? send(200, 'application/json', JSON.stringify(one)) : send(404, 'application/json', '{}'); }
  if (url.pathname === '/api/v1/new-imports') return send(200, 'application/json', JSON.stringify(newImports(url.searchParams.get('kind'))));
  let m = url.pathname.match(/^\/fixture\/(poster|backdrop)\/(\d+)\.svg$/);
  if (m) return send(200, TYPES['.svg'], m[1] === 'poster' ? svg(342, 513, HUES[(m[2] * 3) % HUES.length], +m[2] % 100 + 1) : svg(1280, 720, HUES[m[2] % HUES.length], ''));
  const file = path.join(APP, path.normalize(url.pathname === '/' ? '/index.html' : url.pathname));
  if (!file.startsWith(APP) || !fs.existsSync(file) || fs.statSync(file).isDirectory()) return send(404, 'text/plain', 'not found');
  send(200, TYPES[path.extname(file)] || 'application/octet-stream', fs.readFileSync(file));
});

await new Promise((r) => server.listen(0, '127.0.0.1', r));
const base = `http://127.0.0.1:${server.address().port}`;
fs.mkdirSync(OUT, { recursive: true });
// Pictures from an earlier run would sit beside the new ones under old names.
for (const f of fs.readdirSync(OUT)) if (f.endsWith('.png')) fs.unlinkSync(path.join(OUT, f));

// Installed Chrome by default; PW_CHANNEL= uses Playwright's own browser.
const browser = await chromium.launch(process.env.PW_CHANNEL === '' ? {} : { channel: process.env.PW_CHANNEL || 'chrome' });
const page = await browser.newPage({ viewport: { width: 1920, height: 1080 }, deviceScaleFactor: 1 });
const errors = [];
page.on('pageerror', (e) => errors.push(String(e)));
// The %24 is `$WEBAPIS`: the TV swaps it for a real path, the harness serves it as it is.
await page.goto(base + '/index.html');

let n = 0;
const shot = async (name) => {
  n += 1;
  const file = path.join(OUT, `${String(n).padStart(2, '0')}-${name}.png`);
  await page.screenshot({ path: file });
  console.log(path.relative(APP, file));
};
// The remote's keys have codes a keyboard cannot type (Back is 10009).
const press = async (...keys) => {
  for (const k of keys) {
    await page.evaluate((code) => { const e = new Event('keydown', { bubbles: true }); e.keyCode = code; document.dispatchEvent(e); }, k);
    await page.waitForTimeout(30);
  }
};
const view = () => page.evaluate(() => document.querySelector('.view.on').id);

const html = (sel) => page.evaluate((q) => document.querySelector(q).textContent, sel);
const facts = () => page.evaluate(() => ({ opened: window.__opened, audio: window.__audio, pos: window.webapis.avplay.getCurrentTime(), state: window.webapis.avplay.getState() }));
const checks = [];
const check = (what, ok) => checks.push([what, !!ok]);
const many = async (key, n) => { for (let i = 0; i < n; i++) await press(key); };

await shot('sign-in-empty');
await page.fill('#f-server', base); await page.fill('#f-user', 'robin'); await page.fill('#f-pass', 'wrong');
await page.evaluate(() => document.activeElement.blur());
await press(KEY.DOWN, KEY.DOWN, KEY.DOWN, KEY.ENTER);
await page.waitForSelector('#toast.on:not(.info)');
await shot('sign-in-wrong-password');
await page.fill('#f-pass', 'right');
await page.evaluate(() => document.activeElement.blur());
await press(KEY.ENTER);
await page.waitForSelector('#shell.on');
await page.waitForTimeout(500);

// Home
await shot('home');
check('a movie that arrived twice shows once in Recently added', (await page.evaluate(() => document.querySelectorAll('#pane-home .row')[0].querySelectorAll('.tile').length)) === 7);
check('a season that arrived at once is one tile, with its newest episode', (await html('#pane-home .row:nth-child(2) .tile .name small')) === 'S01E10');
await many(KEY.RIGHT, 7);
await press(KEY.DOWN);
await page.waitForTimeout(300);
await shot('home-second-row');

// The menu, then Movies
await press(KEY.BACK);
await shot('menu-focused');
await press(KEY.DOWN);
check('the menu opens Movies', (await html('#grid-name')) === 'Movies');
await press(KEY.RIGHT);
await page.waitForTimeout(400);
await shot('movies');
check('the movie with no file is not listed', (await html('#grid-count')) === '1 of 60');
await many(KEY.RIGHT, 6);
await press(KEY.DOWN, KEY.DOWN, KEY.DOWN);
await page.waitForTimeout(300);
await shot('movies-letter-strip');
const jumped = await html('#grid-title');
check('the letter strip jumps the grid to another letter', (await html('#letters .sel')) !== '#' && jumped.length > 0);
await press(KEY.LEFT, KEY.ENTER);
await page.waitForTimeout(400);
await shot('movie-page');
check('Enter on a movie opens its page', (await view()) === 'detail');

// Player
await press(KEY.ENTER);
await page.waitForTimeout(700);
await shot('player');
check('the player was opened with a movie stream URL and the token', /\/api\/v1\/movies\/m\d+\/editions\/e\d+\/video\?apikey=harness-token$/.test((await facts()).opened));
check('the English track was chosen over the first, Russian one', (await facts()).audio === 2);
await many(KEY.RIGHT, 3);
await shot('player-skipping');
check('three presses of Right show a target thirty seconds on, before the player moves', (await html('#pb-cur')) === '0:30' && (await facts()).pos < 5000);
await page.waitForTimeout(900);
check('the player then makes one seek to that target', Math.abs((await facts()).pos - 30000) < 3000);
await many(KEY.RIGHT, 30);
await page.waitForTimeout(900);
check('a held key skips much further than ten seconds a press', (await facts()).pos > 15 * 60000);
await press(KEY.ENTER);
await page.waitForTimeout(150);
await shot('player-paused');
check('Enter pauses', (await facts()).state === 'PAUSED');
await press(KEY.DOWN);
await shot('player-audio-option');
await press(KEY.ENTER);
check('Enter on the audio option changes the track', (await facts()).audio === 1);
await press(KEY.UP, KEY.ENTER);
check('Enter on the scrub line plays again', (await facts()).state === 'PLAYING');
await page.waitForTimeout(4600);
await shot('player-controls-hidden');
check('the controls hide while a film plays', await page.evaluate(() => document.getElementById('player').className.includes('idle')));
await press(KEY.BACK);
check('Back from the player returns to the movie page', (await view()) === 'detail');
await press(KEY.BACK);
check('Back from the movie page returns to the grid', (await view()) === 'shell');

// TV
await press(KEY.BACK, KEY.DOWN, KEY.RIGHT);
await page.waitForTimeout(400);
await shot('tv');
check('the series with nothing downloaded is not listed', (await html('#grid-count')) === '1 of 30');
await press(KEY.RIGHT, KEY.ENTER);
await page.waitForSelector('#series.on');
await page.waitForTimeout(400);
await shot('series-page');
await press(KEY.DOWN, KEY.DOWN, KEY.DOWN);
await shot('series-page-missing-episode');
await press(KEY.ENTER);
check('an episode with no file does not play', (await view()) === 'series');
await press(KEY.UP, KEY.UP, KEY.UP, KEY.UP, KEY.RIGHT);
await shot('series-page-season-tabs');
await press(KEY.DOWN, KEY.DOWN, KEY.ENTER);
await page.waitForTimeout(700);
await shot('player-episode');
check('the player was opened with an episode stream URL', /\/api\/v1\/series\/show\d+\/episodes\/s\d+e2x2\/video\?apikey=harness-token$/.test((await facts()).opened));
await page.evaluate(() => window.__endStream());
await page.waitForTimeout(700);
check('at the end of an episode the next one plays', /e2x3\/video/.test((await facts()).opened));
await press(KEY.BACK);
check('Back from an episode returns to the series page', (await view()) === 'series');
check('the series page is on the episode that was playing', (await html('#series-episodes .sel .num')) === '3');
await press(KEY.BACK);

// Search
await press(KEY.BACK, KEY.DOWN, KEY.RIGHT);
await shot('search-empty');
// "HA": H is the 8th key, A the 1st.
await press(KEY.DOWN, KEY.RIGHT, KEY.ENTER, KEY.UP, KEY.LEFT, KEY.ENTER);
await page.waitForTimeout(400);
await shot('search-results');
check('typing on the letter grid finds movies and series', (await html('#search-text')).startsWith('HA') && (await page.evaluate(() => document.querySelectorAll('#search-results .tile').length)) > 0);
await many(KEY.RIGHT, 5);
await press(KEY.RIGHT);
await shot('search-result-focused');
await press(KEY.ENTER);
await page.waitForTimeout(400);
check('Enter on a result opens it', ['detail', 'series'].includes(await view()));
await press(KEY.BACK);
check('Back from a result returns to the search, with what was typed', (await view()) === 'shell' && (await html('#search-text')).startsWith('HA'));
await many(KEY.LEFT, 6);
await many(KEY.DOWN, 6);
await shot('search-wide-keys');
await press(KEY.RIGHT, KEY.RIGHT, KEY.ENTER);
check('Clear empties the search', (await html('#search-text')) === 'Search movies and TV');
check('no JavaScript errors', errors.length === 0);

await browser.close();
server.close();
let failed = 0;
for (const [what, ok] of checks) { console.log(`${ok ? 'ok  ' : 'FAIL'} ${what}`); if (!ok) failed += 1; }
if (errors.length) console.log(errors.join('\n'));
process.exit(failed ? 1 : 0);

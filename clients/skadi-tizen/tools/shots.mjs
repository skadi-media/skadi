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
    setDisplayRect: function () {}, setListener: function (l) { listener = l; },
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
  if (url.pathname === '/api/v1/movies') return send(200, 'application/json', JSON.stringify(movies()));
  let m = url.pathname.match(/^\/fixture\/(poster|backdrop)\/(\d+)\.svg$/);
  if (m) return send(200, TYPES['.svg'], m[1] === 'poster' ? svg(342, 513, HUES[m[2] % HUES.length], +m[2] + 1) : svg(1280, 720, HUES[m[2] % HUES.length], ''));
  const file = path.join(APP, path.normalize(url.pathname === '/' ? '/index.html' : url.pathname));
  if (!file.startsWith(APP) || !fs.existsSync(file) || fs.statSync(file).isDirectory()) return send(404, 'text/plain', 'not found');
  send(200, TYPES[path.extname(file)] || 'application/octet-stream', fs.readFileSync(file));
});

await new Promise((r) => server.listen(0, '127.0.0.1', r));
const base = `http://127.0.0.1:${server.address().port}`;
fs.mkdirSync(OUT, { recursive: true });

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

await shot('sign-in-empty');
await page.fill('#f-server', base); await page.fill('#f-user', 'robin'); await page.fill('#f-pass', 'wrong');
await page.evaluate(() => document.activeElement.blur());
await press(KEY.DOWN, KEY.DOWN, KEY.DOWN, KEY.ENTER);
await page.waitForSelector('#toast.on:not(.info)');
await shot('sign-in-wrong-password');
await page.fill('#f-pass', 'right');
await page.evaluate(() => document.activeElement.blur());
await press(KEY.ENTER);
await page.waitForSelector('#browse.on');
await page.waitForTimeout(400);
await shot('movies');
await press(KEY.RIGHT, KEY.RIGHT, KEY.RIGHT, KEY.RIGHT, KEY.DOWN, KEY.DOWN, KEY.DOWN);
await page.waitForTimeout(300);
await shot('movies-scrolled');
for (let i = 0; i < 12; i++) await press(KEY.DOWN);
await page.waitForTimeout(300);
await shot('movies-last-row');
for (let i = 0; i < 12; i++) await press(KEY.UP);
await press(KEY.LEFT, KEY.LEFT, KEY.LEFT, KEY.LEFT, KEY.ENTER);
await page.waitForTimeout(400);
await shot('movie-page-two-files');
await press(KEY.BACK, KEY.RIGHT, KEY.RIGHT, KEY.RIGHT, KEY.RIGHT, KEY.ENTER);
await page.waitForTimeout(400);
await shot('movie-page-long-title');
await press(KEY.ENTER);
await page.waitForTimeout(600);
await shot('player-bar');
await press(KEY.RIGHT, KEY.RIGHT, KEY.ENTER);
await page.waitForTimeout(400);
await shot('player-after-skip');
await press(KEY.RIGHT, KEY.ENTER);
await page.waitForTimeout(200);
await shot('player-audio-switched');
await page.waitForTimeout(5300);
await shot('player-bar-hidden');

// Not pictures: the flow facts the pictures cannot show.
const facts = await page.evaluate(() => ({ opened: window.__opened, audio: window.__audio, pos: window.webapis.avplay.getCurrentTime() }));
const checks = [
  ['the player was opened with the stream URL and token', /\/api\/v1\/movies\/m4\/editions\/e4\/video\?apikey=harness-token$/.test(facts.opened)],
  ['the English track was chosen over the first, Russian one', facts.audio === 1 || facts.audio === 2],
  ['the skip moved the position by ten minutes', facts.pos >= 600000],
  ['the movie with no file is not listed', !(await page.evaluate(() => document.body.innerHTML.includes('Not Downloaded Yet')))],
];
await press(KEY.BACK);
checks.push(['Back from the player returns to the movie page', (await view()) === 'detail']);
await press(KEY.BACK);
checks.push(['Back from the movie page returns to the movies', (await view()) === 'browse']);
checks.push(['no JavaScript errors', errors.length === 0]);

await browser.close();
server.close();
let failed = 0;
for (const [what, ok] of checks) { console.log(`${ok ? 'ok  ' : 'FAIL'} ${what}`); if (!ok) failed += 1; }
if (errors.length) console.log(errors.join('\n'));
process.exit(failed ? 1 : 0);

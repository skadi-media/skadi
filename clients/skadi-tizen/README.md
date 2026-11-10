# Skadi for Samsung TV (Tizen)

A player for the TV: sign in, browse movies, play. It is a Tizen web app that
hands the file's URL to the TV's own player (AVPlay), which decodes it in
hardware. Skadi does not transcode, so this is what makes direct play work.

Status: a first slice. Movies only. No series, resume, subtitles or search yet.

## What it runs on

Built and tested on a 2017 set (Tizen 3.0). That sets the rules for the code:

- The web engine is Chromium M47. Plain ES5 JavaScript: no modules, no
  `async`/`await`, no arrow functions. CSS has flexbox but no grid and no
  custom properties. There is no build step and no transpiler.
- WebAssembly is not available, so the Rust web client cannot run here.

## Layout

| Path | What |
|---|---|
| `config.xml` | The Tizen manifest. The package id must be exactly 10 characters. |
| `index.html`, `css/` | The four views: sign in, movies, movie page, player. |
| `js/core.js` | Pure logic, shared by the TV and the tests. |
| `js/app.js` | Views, remote keys, the player. |
| `test/` | Unit tests for `core.js`. |
| `tools/build.sh` | Packages `build/Skadi.wgt`. |
| `tools/sdb-relay.py` | Workaround for `sdb connect` failing on macOS. |

## Tests

```sh
node --test test/
```

## Toolchain

1. Install the Tizen Studio CLI (the `web-cli` installer from
   <https://download.tizen.org/sdk/Installer/>). On Apple Silicon it runs under
   Rosetta. Put `tools/ide/bin` and `tools` of the install on `PATH`.
2. Make a signing profile. A local Tizen certificate is enough; no Samsung
   account is needed for a 2017 set:

   ```sh
   tizen certificate -a skadidev -p <password> -f skadidev
   tizen security-profiles add -n skadidev -a <path to skadidev.p12> -p <password>
   ```

## The TV

1. On the TV open **Apps**, press `1 2 3 4 5` on the remote, turn developer
   mode on and enter the address of the computer that will install the app.
   Restart the TV fully (hold the power button).
2. Connect:

   ```sh
   sdb connect <tv-address>
   ```

   On macOS this can fail with `failed to connect to remote target` while the
   TV is reachable. Then relay through a local port:

   ```sh
   tools/sdb-relay.py <tv-address> 36101 &
   sdb connect 127.0.0.1:36101
   ```

## Build, install, run

```sh
tools/build.sh                       # build/Skadi.wgt, signed with profile `skadidev`
tizen install -n Skadi.wgt -t <target> -- build
tizen run -p SkadiTizen.Skadi -t <target>
```

`<target>` is the name `sdb devices` shows. An install over a copy signed with
another certificate fails: uninstall the old one first.

## Using it

- **Sign in** once with the server address (`192.0.2.10:8090`), a household
  username and its password. The TV keeps a device token, not the password.
- **Movies**: the arrows move, Enter opens a movie, fast-forward and rewind
  jump several pages.
- **Player**: any key shows the bar. Left and Right choose a button, Enter
  presses it. The play, pause, fast-forward and rewind keys work without the
  bar. Back stops.

## Things the TV taught us

- **Header names must be Title-Case.** The TV's HTTP client reads response
  header names case-sensitively. With `accept-ranges` and `content-range` in
  lowercase it plays a file but refuses every seek. The server sends
  Title-Case names for this reason (SKADI-T-0756).
- **One player call at a time.** After `seekTo`, no other AVPlay call until its
  callback fires. Some files never answer a seek, so the app also times out.
- **The player takes only a URL**, no headers, so the token is in the query
  string (`?apikey=`).
- **The first audio track is not always the right one.** The app picks by
  language, then by what the TV decodes. TrueHD, FLAC and Opus are not decoded
  by a 2017 set.
- **Old-style VC-1 in MKV plays but does not seek.**

## Local defaults for development

`js/dev.local.js` is git-ignored and optional. It can set `window.SKADI_DEV`:

```js
window.SKADI_DEV = {
  server: 'http://192.0.2.10:8090', // prefill the server field
  user: 'robin',                    // prefill the username
  log: function (msg) { /* send the app's log somewhere */ },
  videoUrl: function (server, movieId, editionId, token, container) { /* another stream URL */ }
};
```

`tools/build.sh` packages it when it is there, so do not hand such a build to
anyone else.

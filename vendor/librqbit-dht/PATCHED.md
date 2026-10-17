# librqbit-dht 5.3.1, patched for skadi

A copy of [librqbit-dht](https://github.com/ikatson/rqbit) 5.3.1 from crates.io,
Apache-2.0 (see the `license` field in `Cargo.toml`). The downloader worker uses
it through `[patch.crates-io]` in `crates/skadi-downloader-worker/Cargo.toml`.

It fixes an unbounded memory growth in the worker (SKADI-T-0645). Each torrent
runs a peer search forever, and with hundreds of torrents the queued DHT requests
outgrew the session's rate limiter: each held a future and a tracing span, and
the worker grew about 29 MiB a minute until it hit its memory cap.

The changes, each marked `skadi patch (SKADI-T-0645)` in `src/dht.rs`:

1. `request_one` sends each request once. Upstream sent it a second time.
2. `DhtState::request` bounds the wait for the rate limiter by
   `RESPONSE_TIMEOUT`. Upstream waited without limit before the timeout began.
3. A peer search keeps at most `MAX_INFLIGHT_PER_SEARCH` (32) requests in
   flight; a node heard of beyond that is dropped and found again on the next
   requery.

librqbit-dht 9.0.1 (2026-08-20) has the same code. Drop this copy when upstream
fixes it.

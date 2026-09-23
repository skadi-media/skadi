-- skadi-audiobooks: watchers (SKADI-I-0018 / SKADI-T-0156). A watcher is the
-- acquisition switch, applied at author / series / book scope. A known-works
-- entry covered by an active watcher and not already owned is materialised as a
-- monitored Missing book for the hunter to acquire. "Know" (works) is decoupled
-- from "acquire" (watchers).
--
--   scope_kind: 'author' | 'series' | 'book'
--   scope_key : author ASIN | series ASIN | work (book) ASIN

CREATE TABLE watchers (
    scope_kind  TEXT NOT NULL,
    scope_key   TEXT NOT NULL,
    created_at  TIMESTAMP NOT NULL,
    PRIMARY KEY (scope_kind, scope_key)
);

CREATE INDEX watchers_kind_idx ON watchers(scope_kind);

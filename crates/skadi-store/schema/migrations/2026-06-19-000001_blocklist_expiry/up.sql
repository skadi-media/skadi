-- Blocklist TTL / expiry (SKADI-T-0198): an optional expiry so a temporary block
-- (e.g. a tracker hiccup) stops vetoing after a while, instead of parking a
-- release forever. NULL = permanent (the default for a manual block).
ALTER TABLE blocklist ADD COLUMN expires_at TIMESTAMP;

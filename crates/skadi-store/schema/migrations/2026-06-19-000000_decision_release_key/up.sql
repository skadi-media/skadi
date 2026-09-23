-- Grab→import correlation (SKADI-T-0196): the stable blocklist identity
-- (`release_key`) of the chosen release, so a History/Activity row can correlate
-- to its grab and offer "blocklist-and-search" without re-searching. Nullable —
-- older rows (and any decision recorded before this column existed) carry NULL.
ALTER TABLE decision_history ADD COLUMN release_key TEXT;

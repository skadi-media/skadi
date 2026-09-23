-- skadi-audiobooks (SKADI-T-0160): cache the work's language so the body-of-
-- work / series catalog can be filtered to the operator's language (English).
-- Nullable: unknown until fetched from the Audible per-title detail endpoint;
-- unknown is treated as "show" so a transient lookup failure never hides a work.

ALTER TABLE works ADD COLUMN language TEXT;

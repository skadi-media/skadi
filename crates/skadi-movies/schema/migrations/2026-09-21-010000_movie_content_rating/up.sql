-- US MPAA certification from the metadata source (SKADI-T-0610), e.g. "PG-13".
-- NULL until a refresh reports one; household policies treat NULL as unrated.
ALTER TABLE movies ADD COLUMN content_rating TEXT;

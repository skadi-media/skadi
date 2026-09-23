-- US TV parental guideline from the metadata source (SKADI-T-0610), e.g. "TV-14".
-- NULL until a refresh reports one; household policies treat NULL as unrated.
ALTER TABLE series ADD COLUMN content_rating TEXT;

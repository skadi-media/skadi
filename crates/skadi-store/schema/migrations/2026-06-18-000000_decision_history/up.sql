-- Persisted decision history (SKADI-T-0187): the chosen release's full decision
-- explanation at grab time, so the UI can show *why* a past acquire happened
-- without re-searching. Cross-domain (keyed by the opaque `acquirable_ref`);
-- `explanation` holds the serialized `ReleaseExplanation` JSON, with a few
-- columns denormalized for cheap list rendering. Append-only; the live "why"
-- still ships via the `…/quality/test` + `…/releases` endpoints.

CREATE TABLE decision_history (
    id TEXT PRIMARY KEY NOT NULL,
    at TIMESTAMP NOT NULL,
    kind TEXT NOT NULL,
    acquirable_ref TEXT NOT NULL,
    title TEXT NOT NULL,
    quality TEXT,
    decision TEXT,
    format_score INTEGER NOT NULL,
    explanation TEXT NOT NULL
);

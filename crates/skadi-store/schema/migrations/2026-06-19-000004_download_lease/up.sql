-- Claim lease (SKADI-T-0214): when a tracked download's lease lapses (the worker
-- died without un-claiming), the periodic reclaim returns the row to `queued`.
-- NULL until the worker first heartbeats after claiming.
ALTER TABLE downloads ADD COLUMN lease_expires_at TIMESTAMP;

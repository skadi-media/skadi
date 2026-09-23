-- When a download finished downloading and began seeding (SKADI-T-0210), so the
-- worker can enforce seed-time limits (seeded_secs = now - completed_at). NULL
-- until the job completes.
ALTER TABLE downloads ADD COLUMN completed_at TIMESTAMP;

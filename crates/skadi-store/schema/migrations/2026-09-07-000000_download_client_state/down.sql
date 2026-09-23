-- Reverses the `client_state` column (SKADI-T-0394).
--
-- This file was missing, so the generator emitted an empty `down.sql`.
-- SQLite shrugs at an empty statement; Postgres refuses it with "Received an
-- empty query", which broke the cross-backend migration round-trip — invisible
-- until CI reached that test for the first time on 2026-09-23.
ALTER TABLE downloads DROP COLUMN client_state;

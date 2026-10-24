-- An `imported` row that replaced a file the library already held
-- (SKADI-T-0743): the new-imports list shows it with an "Upgrade" pill. The
-- same fact as the Upgraded notification (SKADI-T-0538). False for every other
-- row, and for rows written before this column.
ALTER TABLE acquisition_history ADD COLUMN upgrade BOOLEAN NOT NULL DEFAULT FALSE;

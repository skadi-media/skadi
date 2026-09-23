-- `book_files` was left in place by the up migration, so reverting only drops
-- what was added. Editions beyond Unabridged are lost, which is inherent: the
-- old shape cannot hold them.
DROP TABLE book_editions;
DROP TABLE book_edition_kinds;

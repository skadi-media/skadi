# skadi-store schema — logical DDL workflow

skadi-store and skadi-movies target **both** SQLite and Postgres through
[`diesel-dualdb`](https://github.com/colliery-io/diesel-dualdb). Instead of
hand-writing a `table!` and a migration per backend, we write each table **once**
in *logical* DDL and generate everything from it.

## Layout

```
crates/<crate>/schema/
  migrations/<timestamp>_<name>/up.sql   # ← the source of truth (logical DDL)
  generated/                              # ← generated; committed; do NOT hand-edit
    schema.rs                             #    one table! per table, portable types
    migrations-postgres/<name>/{up,down}.sql
    migrations-sqlite/<name>/{up,down}.sql
```

`src/schema.rs` is just `include!("../schema/generated/schema.rs")`, and the
crate's `SQLITE_MIGRATIONS` / `POSTGRES_MIGRATIONS` `embed_migrations!` at the
generated trees.

## Adding or changing a table

1. Edit (or add a new timestamped migration under) `schema/migrations/` using
   **logical** column types (left column below).
2. Regenerate: `angreal db schema`.
3. Build/test; commit the `schema/migrations` change **and** the regenerated
   `schema/generated` tree together.

`angreal db schema --check` (wire it into CI) regenerates to a temp dir and
fails if the committed `generated` tree has drifted from the logical DDL.

> The generator binary (`diesel-dualdb-schema`, from `diesel-dualdb-cli`) lives
> in the sibling `diesel-dualdb` checkout until it's published. The angreal task
> finds it on `PATH` or at `../diesel-dualdb/target/debug/`; if absent it's a
> no-op with a message.

## Logical → backend types

| Logical DDL | Postgres   | SQLite        | Rust column / value type            |
| ----------- | ---------- | ------------- | ----------------------------------- |
| `TEXT`      | `text`     | `TEXT`        | `String` (all ids are TEXT here)    |
| `BIGINT`    | `bigint`   | `BIGINT`      | `i64`                               |
| `INTEGER`   | `integer`  | `INTEGER`     | `i32`                               |
| `BOOLEAN`   | `boolean`  | `INTEGER`     | `bool`                              |
| `TIMESTAMP` | `timestamptz` | `TEXT` (RFC3339 UTC) | `diesel_dualdb::types::Timestamp` (`DateTime<Utc>`) |
| `JSON`      | `jsonb`    | `TEXT`        | `diesel_dualdb::types::Json<T>`     |
| `BYTEA`     | `bytea`    | `BLOB`        | `diesel_dualdb::types::Bytes` (`Vec<u8>`) |

## Gotchas

- **Reserved words** (e.g. a column named `key`) must be quoted in the logical
  DDL: `"key" TEXT PRIMARY KEY NOT NULL`.
- **Backend-specific DDL** (seed `INSERT`s that differ, `ON CONFLICT` vs
  `INSERT OR IGNORE`, GIN indexes, …) goes in a verbatim escape block:
  `-- dualdb:postgres` … `-- dualdb:end` / `-- dualdb:sqlite` … `-- dualdb:end`.
- **Unique migration versions across crates.** diesel tracks applied migrations
  by the timestamp prefix in one `__diesel_schema_migrations` table; if
  skadi-store and skadi-movies share a version string the second is silently
  skipped. Keep their timestamps distinct.
- **`ON CONFLICT` in queries** isn't expressible through `MultiBackend` — use the
  `conn.dispatch(|pg| …, |sqlite| …)` escape hatch (see any repo's upsert).

# Contributing to Skadi

## Code Style

- Run `cargo fmt` before committing
- Run `cargo clippy --workspace -- -D warnings` and fix all warnings
- No emojis in code, commits, or docs
- Doc comments on all public APIs

## Commit Messages

- Use imperative mood ("add feature" not "added feature")
- Keep the subject line under 50 characters
- No trailing period in subject line
- Reference issues where applicable

## Testing

- Write unit tests for new functionality
- Ensure integration tests pass
- Run the full test suite before submitting: `angreal test all`

## Database schema (SQLite + Postgres)

skadi is dual-backend via [`diesel-dualdb`](https://github.com/colliery-io/diesel-dualdb).
**Do not hand-write per-backend `table!`s or migrations.** Each table is written
once in *logical* DDL under `crates/<crate>/schema/migrations/`; the `schema.rs`
and per-backend migration trees in `schema/generated/` are produced by
`angreal db schema` and committed. To change the schema: edit the logical DDL,
run `angreal db schema`, then commit the migration **and** the regenerated tree
together. `angreal db schema --check` fails on drift (CI guard). See
[`crates/skadi-store/schema/README.md`](crates/skadi-store/schema/README.md) for
the type mapping and gotchas (reserved words, escape blocks, upserts via
`conn.dispatch`).

## Pull Requests

- Keep PRs focused on a single change
- Include tests for new functionality
- Update documentation as needed

CREATE TABLE watchers (scope_kind TEXT NOT NULL, scope_key TEXT NOT NULL, created_at TIMESTAMPTZ NOT NULL, PRIMARY KEY (scope_kind, scope_key));

CREATE INDEX watchers_kind_idx ON watchers(scope_kind);

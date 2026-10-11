# KSI Backend — Rust port

A faithful Rust rewrite of [fi-ksi/web-backend](https://github.com/fi-ksi/web-backend)
(Python/Falcon), implemented with [axum](https://github.com/tokio-rs/axum) and
[sqlx](https://github.com/launchbadge/sqlx).

## Why

- **Performance**: the Python backend is single-threaded per worker; the Rust
  port is fully async on tokio.
- **Correctness**: the test suite validates every response against the
  [web-backend-swagger](https://github.com/fi-ksi/web-backend-swagger) OpenAPI
  spec — the spec is the contract.
- **Safety**: no more silent `None` handling; the type system enforces it.

## Status

This is a working port of the core API. Implemented endpoints:

| Group | Endpoints |
|---|---|
| Years | `GET /years`, `GET /years/{id}` (incl. the #225 fix — unreleased tasks excluded) |
| Waves | `GET /waves`, `GET /waves/{id}` |
| Tasks | `GET /tasks`, `GET /tasks/{id}` (visibility rules for non-orgs) |
| Articles | `GET /articles`, `GET /articles/{id}` |
| Auth | `POST /auth` (password grant), `GET /logout` |
| Posts | `POST /posts`, `GET /posts/{id}`, `POST/DELETE /posts/{id}/resolve` (the #182 resolved-tracking feature) |
| Threads | `GET /threads`, `GET /threads/{id}`, `GET /threadDetails/{id}` |
| Admin | `GET /admin/subm/task/{id}` (the #191 fix — 404 instead of empty ZIP, warnings for missing files, `SubmittedCode` included) |
| Misc | `GET /robots.txt`, `GET /csp` |

Not yet ported: module submission/evaluation, user profiles, registrations,
corrections workflow, task deploy, e-mail sending, diplomas. These follow the
same patterns established here.

### Notable fixes ported from the Python backend

- **#225**: `GET /years` aggregates only count tasks from published waves.
- **#191**: `GET /admin/subm/task/{id}` returns 404 with a JSON error instead
  of a silent empty ZIP; missing files are logged as warnings; programming
  module answers (`SubmittedCode`) are included as `code_<id>.txt`.
- **#182**: posts carry `resolved_at`; an org's reply auto-resolves the
  parent, a participant's reply reopens it; orgs can resolve/reopen manually.

### Known spec discrepancies

The validator skips (with a notice) operations missing from the swagger spec
or with unresolvable `$ref`s:

- `POST /auth` → broken `$ref` to `#/components/schemas/AuthResponse` in the spec.
- `POST /posts/{id}/resolve`, `DELETE /posts/{id}/resolve` → new in #182, not yet in the spec.
- `GET /articles` → the spec says `resource`, the real backend returns `content`.

## Running

```bash
# build
cargo build --release

# run (SQLite by default; migrations run automatically)
DATABASE_URL=sqlite://ksi.db LISTEN_ADDR=127.0.0.1:3030 ./target/release/ksi-backend
```

Environment variables:

| Variable | Default | Description |
|---|---|---|
| `DATABASE_URL` | `sqlite://ksi.db` | Database URL (`sqlite://…` or `sqlite::memory:`) |
| `LISTEN_ADDR` | `127.0.0.1:3030` | Address to listen on |
| `CORS_ORIGIN` | _(none)_ | Value for `Access-Control-Allow-Origin` |

The database schema is created from `crates/ksi-backend/migrations/001_initial.sql`,
generated from the Python SQLAlchemy models, so an existing Python-backend
database works as-is.

## Testing

```bash
# all tests (unit + integration, with swagger-spec validation)
cargo test

# with the swagger spec at a custom location
SWAGGER_JSON=/path/to/swagger.json cargo test
```

The integration tests (`crates/ksi-backend/tests/`) spin up the real axum
router against a fresh in-memory SQLite database, seed realistic data, and:

1. assert HTTP status codes and response shapes,
2. validate every response against the swagger spec's JSON schema
   (`tests/common/mod.rs::assert_swagger_valid`),
3. cover auth (login, wrong password, token expiry, logout invalidation),
4. cover the #225, #191 and #182 regression scenarios.

## API compatibility

- Auth: `Authorization: Bearer <token>` with opaque tokens stored in
  `oauth2_tokens`, exactly like the Python backend (same table, so tokens
  issued by either implementation work in the other).
- Errors: `{"errors": [{"status", "title", "detail"}]}` envelope, same as Python.
- Passwords: bcrypt, same hashes as the Python backend.

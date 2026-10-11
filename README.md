# Backend for Online Seminar of Informatics

[ksi.fi.muni.cz](https://ksi.fi.muni.cz/)

Rust rewrite (axum + sqlx) of the original Python/Falcon backend,
organized as a multi-crate monorepo.

## Crates

| Crate | Description |
|---|---|
| `ksi-models` | DB entities + API JSON types, split by domain (`user`, `task`, `post`, …) |
| `ksi-db` | Connection pool + embedded SQL migrations |
| `ksi-auth` | Token lookup, password hashing, axum `AuthUser` extractor |
| `ksi-api` | HTTP routes, error types, `AppState`, integration tests |
| `ksi-sandbox` | Code-execution sandbox: `Sandbox` trait + `BwrapSandbox` (Bubblewrap, Cauldron-style per the [MUNI thesis](https://is.muni.cz/th/daghp/)), merge/check-script pipeline, honeypot cheat detection |
| `ksi-backend` | The binary (wires everything together) |

Dependency versions are centralized in the workspace root `Cargo.toml`
(`[workspace.dependencies]`), and the toolchain is pinned in
`rust-toolchain.toml`.

## Quickstart (development)

Prerequisites: [Rust](https://rustup.rs/) (install via `rustup`; the correct
toolchain is picked up automatically from `rust-toolchain.toml`).

```bash
# 1. configure (optional — defaults work out of the box)
cp .env.example .env

# 2. run (migrations run automatically on startup)
cargo run -p ksi-backend

# backend listens at http://127.0.0.1:3030
```

Or with docker:

```bash
docker compose up --build
# backend at http://localhost:3030
```

### Configuration

| Variable | Default | Description |
|---|---|---|
| `LISTEN_ADDR` | `127.0.0.1:3030` | Address to listen on |
| `DATABASE_URL` | `sqlite://ksi.db` | Database URL (`sqlite://…` or `sqlite::memory:`) |
| `CORS_ORIGIN` | _(none)_ | Value for `Access-Control-Allow-Origin` |
| `KSI_SALT` | _(none)_ | Salt for the `edulint_source_id` user hash |
| `KSI_SEMINAR_NAME_SHORT` | `KSI` | Seminar short name in `edulint_source_id` |
| `KSI_BOX_PREFIX` | `1` | Sandbox box-id prefix (change when several backends share a host) |
| `KSI_MAX_CONCURRENT_EXEC` | `3` | Max concurrently running sandbox boxes |
| `KSI_SUBMISSIONS_DIR` | `data/submissions` | Where uploaded solution files are stored |
| `KSI_MODULE_LIB_DIR` | `data/module_lib` | Module library passed to merge/check scripts |
| `KSI_EXEC_STORE_DIR` | `data/exec` | Where finished sandbox boxes are archived |

The database schema is created automatically from `crates/ksi-db/migrations/`
on startup, so an existing Python-backend database works as-is.

## Testing

```bash
# all tests (unit + integration, with swagger-spec validation)
cargo test

# with the swagger spec at a custom location
SWAGGER_JSON=/path/to/swagger.json cargo test
```

The integration tests (`crates/ksi-api/tests/`) spin up the real axum
router against a fresh in-memory SQLite database and validate every response
against the [web-backend-swagger](https://github.com/fi-ksi/web-backend-swagger)
OpenAPI spec.

## API status

| Group | Endpoints |
|---|---|
| Years | `GET /years`, `GET /years/{id}` |
| Waves | `GET /waves`, `GET /waves/{id}` |
| Tasks | `GET /tasks`, `GET /tasks/{id}` |
| Articles | `GET /articles`, `GET /articles/{id}` |
| Auth | `POST /auth`, `GET /logout` |
| Posts | `POST /posts`, `GET /posts/{id}`, `POST`/`DELETE /posts/{id}/resolve` |
| Threads | `GET /threads`, `GET /threads/{id}`, `GET /threadDetails/{id}` |
| Modules | `GET /modules/{id}`, `POST /modules/{id}/submit`, `POST /modules/{id}/submitFiles`, `POST /runCode/{moduleId}/submit` |
| Admin | `GET /admin/subm/task/{id}` |

Not yet ported: user profiles, registrations,
corrections workflow, task deploy, e-mail, diplomas.

### Code execution sandbox

Programming-module evaluation (`POST /modules/{id}/submit` with
`autocorrect`, `POST /runCode/{moduleId}/submit`) runs participant code
through a [Bubblewrap](https://github.com/containers/bubblewrap)-based
sandbox, following the design from the MUNI bachelor's thesis
"Automated Evaluation of Student Submissions in Computer Science
Correspondence Seminar" ([thesis](https://is.muni.cz/th/daghp/),
Cauldron): Linux namespaces (user, mount, PID, IPC, UTS, cgroup, network),
all capabilities dropped, resource limits via `setrlimit(2)`, size-limited
tmpfs, and read-only bind mounts. A free box id is picked under
`/tmp/ksi-bwrap/` (at most 3 concurrent boxes), code is merged with the
module's merge script, executed with the module's resource limits, and
checked with the module's check script.

**The `bwrap` binary must be installed for code execution to work.**
When it is missing, the sandbox returns a clean "not available" error
and the API responds with `503 Service Unavailable` instead of crashing.
Everything else (quiz/sortable/text evaluation, file uploads, module
details) works without it. The `Dockerfile` does not install `bwrap`
yet — add it there if you deploy evaluation.

### Compatibility notes

- Auth: `Authorization: Bearer <token>` with opaque tokens in `oauth2_tokens`
  (same table — tokens are interchangeable with the Python backend).
- Errors: `{"errors": [{"status", "title", "detail"}]}` envelope.
- Passwords: bcrypt, same hashes as before.

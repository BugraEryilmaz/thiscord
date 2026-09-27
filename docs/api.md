# API contracts

`shared` owns Serde transport types. Database models and HTTP status mappings
stay on the backend. Existing liveness JSON remains backward compatible.

Account commands/responses are defined in `shared/src/account.rs`; see
[account API, sessions and provider setup](accounts.md). Authenticated commands
use a bearer header on `POST /api/v1/account`. Account failures use this same
request-correlated error envelope, with `unauthorized`, `forbidden`, `conflict`
and `rate_limited` codes in addition to the foundation codes.

| Route | Result |
| --- | --- |
| `GET /api/v1/health` | 200, `{"status":"ok"}`; process liveness only |
| `GET /api/v1/ready` | 200 after querying the installation record, otherwise 503 |

Readiness success:

```json
{
  "status": "ready",
  "instance_id": "f8f72890-fbae-4e56-9e7b-8038b5c3a094",
  "checked_at": "2026-09-27T12:00:00.123456Z"
}
```

Readiness failure:

```json
{
  "code": "service_unavailable",
  "message": "Database is not ready",
  "request_id": "d26031d4-1c1c-48b2-a415-d576b141898b"
}
```

Every response, including errors and CORS preflight, has `x-request-id`.
The backend creates a new UUID for each request, replaces supplied client IDs,
and attaches it to the tracing span and error body. CORS exposes the header to
allowed UI origins. Logs include method, path, status and duration; query strings,
credentials, SQL errors and request bodies are not logged by the middleware.

`ApiError` uses stable snake_case `ErrorCode` values and optional field details.
Clients should branch on `code`, not human-readable `message`. Unknown routes
and unsupported methods use the same envelope (404/405). Add typed extractor
rejection mapping when introducing JSON/query input endpoints; the current
health/readiness routes have no input payloads.

Readiness checks the actual schema/data, not a cached pool flag. Pool checkout
is bounded at 2 seconds; connections use 2-second statement and 1-second lock
timeouts; HTTP probes have a 3-second deadline. Blocking database operations run
off Tokio workers. A timed-out blocking task cannot be forcibly cancelled by
Tokio; the database timeouts bound its query. Liveness continues to work when
the pool is exhausted or the installation table is missing.

## IDs and timestamps

`InstanceId` and `RequestId` are distinct UUID newtypes with string wire formats.
Add other domain ID types when their entities are introduced. Random ID generation
is a backend concern; `shared` has no random source or platform dependencies.

`Timestamp` is a UTC `DateTime` serialized as RFC 3339. Offset-bearing input is
normalized to UTC; naive timestamps are invalid. Persist timestamps as PostgreSQL
`TIMESTAMPTZ`. Pagination positions use PostgreSQL's microsecond precision.

## Validation and pagination

`validate_text` checks required nonblank text and maximum Unicode scalar count,
without mutating input. `FieldError` contains a field name and stable validation
code, not the rejected value. Domain DTOs must add their own syntax rules; a
generic text check is not sufficient password/email validation. Run validation
on the backend even if the frontend also runs it.

`PageRequest` defaults to 50 items; `PageSize` accepts 1–100 and enforces this
during Serde deserialization. Unknown fields are rejected. `Page<T>` contains
`items` and nullable `next_cursor`.

`PageCursor` is a versioned, URL-safe, unpadded base64 position containing an
8-byte signed UTC microsecond timestamp and a 16-byte UUID after its version byte.
Invalid length, encoding, version and timestamps are rejected. List endpoints
must order ascending by `(created_at, id)`, apply a strict greater-than predicate,
and fetch `limit + 1` rows to determine the next cursor. UUID breaks timestamp
ties. Cursors are positions, not signed authorization tokens; permissions and
filters must be applied on every query. The permission editor currently uses complete
snapshots within enforced installation/guild capacity limits, described in
[permissions.md](permissions.md); larger collections must adopt this pagination.

## Membership and permissions

`POST /api/v1/permissions` uses `PermissionRequest`/`PermissionResponse` from
`shared/src/permissions.rs`. Every command requires an active bearer session. Guild
changes include the last viewed revision; stale revisions return `409 Conflict`.
See [permissions.md](permissions.md) for bootstrap, hierarchy, override precedence
and media enforcement still needed when those handlers are added.

## Chat

`POST /api/v1/chat` and `/api/v1/socket` use shared contracts in `shared/src/chat.rs`.
See [chat.md](chat.md) for authentication, socket versioning, history, deduplication
and reconnect behavior. History uses a reverse keyset `before` cursor to load older
pages, returning each page in ascending order; ordinary forward pagination retains
the strict greater-than convention above.

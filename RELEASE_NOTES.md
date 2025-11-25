## Changes
- Gemini CLI responses now flow through typed schemas for both JSON and SSE; streaming adds keep-alives plus a 60s idle timeout so hung upstreams fail fast.
- Error handling tightened: upstream parse/protocol failures map to 502 with clear codes, upstream HTTP statuses are preserved, and `NO_CREDENTIAL` now returns 409.
- Outbound clients close idle pools when HTTP/1-only, trim Gemini request timeouts to 10 minutes, and expand OAuth refresh retries to 3 to reduce flakes.
- Credential scheduling avoids duplicate queue entries and the SQLite schema drops `AUTOINCREMENT` (uses rowid PK) to avoid UPSERT id gaps; new DBs no longer force WAL.

## Upgrade notes
- No migration required; existing `data.db` continues to work. Restart the service to pick up the new timeout/retry tuning; fresh databases will be created without AUTOINCREMENT/WAL.

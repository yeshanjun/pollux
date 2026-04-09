# Pollux

Pollux is a headless, actor-driven Rust reverse proxy that orchestrates AI resources. It serves as a microsecond-level scheduler, transforming raw credential resources into standard Gemini & OpenAI interfaces.

It is designed to be **stateless at the edge** and **stateful in SQLite**

## Highlights

- **Actor-based scheduling**: built on `ractor` to keep the hot path lock-free.
- **Resources pool & rotation**: retries, rotation on upstream errors, and queue-based scheduling.
- **Streaming support**: SSE passthrough for both Gemini streaming and Codex streaming.
- **Single binary / Docker**: runs as a small container or `cargo run`.

## Authentication

All **proxy endpoints** require a shared key (`basic.pollux_key` in `config.toml`).

Pollux supports all of the following auth forms:

- Header: `Authorization: Bearer <pollux_key>`
- Query: `?key=<pollux_key>`
- Header: `x-goog-api-key: <pollux_key>` (kept for compatibility with Gemini tooling)

Recommended usage (to avoid confusion):

- **Codex endpoints** (`/codex/*`): prefer `Authorization: Bearer <pollux_key>`
- **Gemini CLI endpoints** (`/geminicli/*`): prefer `?key=<pollux_key>`

OAuth entry/callback endpoints do **not** require the key.

## Quick Start

### 1) Configure (`config.toml`)

Template: [`config.toml.example`](./config.toml.example)

`pollux` requires a real `config.toml` at runtime (and `basic.pollux_key` must be non-empty).

Minimal example:

```toml
[basic]
listen_addr = "0.0.0.0"
listen_port = 8188
database_url = "sqlite://data.db"
loglevel = "info"
pollux_key = "change-me"
insecure_cookie = false

[providers.geminicli]
model_list = ["gemini-2.5-pro"]

[providers.codex]
model_list = ["gpt-5.2-codex"]
```

`basic.insecure_cookie` defaults to `false` (recommended for HTTPS).
If you access Pollux via plain HTTP (for testing), set it to `true`; otherwise browser OAuth session cookies may not be sent.

### 2) Run

**Option A: [Docker Compose]**

Template: [`docker-compose.yml.example`](./docker-compose.yml.example)

- Copy `docker-compose.yml.example` to `docker-compose.yml`
- Make sure your `config.toml` is mounted to `/app/config.toml`
- Start: `docker compose up -d`

**Option B: Local**

```bash
cargo run --release
```

Server defaults to `0.0.0.0:8188` (configurable).

## Onboarding Credentials

### Gemini CLI (Google)

**Method A: OAuth (browser)**

1. Open `http://localhost:8188/geminicli/auth`
2. Complete Google OAuth
3. You should see `Success` from `/oauth2callback`

**Method B: Refresh token ingestion**

```bash
curl -X POST "http://localhost:8188/geminicli/resource:add?key=change-me" \
  -H "Content-Type: application/json" \
  -d '[{"refresh_token":"1//..."}, {"refresh_token":"2//..."}]'
```

Pollux returns `202 Accepted` + `Success` once accepted; detailed validation outcomes are logged.

### Codex (OpenAI)

**Method A: OAuth (browser)**

1. Open `http://localhost:8188/codex/auth`
2. Complete OpenAI OAuth
3. OpenAI redirects to `http://localhost:1455/auth/callback?...` (Codex CLI default)
4. If Pollux is not listening on `1455`, change the port in the browser address bar to your Pollux port
   (e.g. `8188`) and reload.

**Method B: Refresh token ingestion**

```bash
curl -X POST "http://localhost:8188/codex/resource:add" \
  -H "Authorization: Bearer change-me" \
  -H "Content-Type: application/json" \
  -d '[{"refresh_token":"rt_01..."}, {"refresh_token":"rt_02..."}]'
```

## License

See [LICENSE](./LICENSE). This project is licensed under the GNU Affero General Public License v3.0.

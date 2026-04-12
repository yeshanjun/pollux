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

## License

See [LICENSE](./LICENSE). This project is licensed under the GNU Affero General Public License v3.0.

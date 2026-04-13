# Pollux

Pollux is a headless, actor-driven Rust reverse proxy that orchestrates AI resources. It serves as a microsecond-level scheduler, transforming raw credential resources into standard Gemini & OpenAI interfaces.

It is designed to be **stateless at the edge** and **stateful in SQLite**

## Highlights

- **Actor-based scheduling**: built on `ractor` to keep the hot path lock-free.
- **Resources pool & rotation**: retries, rotation on upstream errors, and queue-based scheduling.
- **Streaming support**: SSE passthrough for both Gemini streaming and Codex streaming.
- **Single binary / Docker**: runs as a small container or `cargo run`.

## License

See [LICENSE](./LICENSE). This project is licensed under the GNU Affero General Public License v3.0.

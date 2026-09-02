# Rust migration: status

Tracks the migration of this repo from the Python FastMCP bridge to Rust, using
the official [A2A Rust SDK](https://github.com/a2aproject/a2a-rs) (`a2a`/`a2a-client`)
and the official [MCP Rust SDK](https://github.com/modelcontextprotocol/rust-sdk) (`rmcp`).

Branch: `claude/repo-migration-rust-n2x1xv` — PR: https://github.com/Gnidreve/A2A-MCP-Server/pull/1
(open against `main`, not yet merged).

## Agreed scope (why it looks the way it does)

- **The bridge is a pure MCP server + A2A client. Not an A2A server.** MCP is
  inherently request/response, initiated by the MCP client — there is no
  scenario in this deployment where something needs to call *into* the bridge
  over A2A. The old Python `CustomA2AServer`/`setup_a2a_server` code was dead
  code anyway (it never actually served anything). Do not resurrect an A2A
  server role without a concrete new requirement.
- **Every agent the bridge talks to must already run its own A2A-v1 server
  (inbound-capable).** The bridge cannot make an arbitrary HTTP endpoint
  "A2A-capable" — `register_agent` fetches `{url}/.well-known/agent-card.json`
  and fails cleanly if that isn't there or doesn't parse.
- **Target deployment is Docker Compose**, supporting multiple registered
  agents (`docker-compose.yml` base + `docker-compose.proxy.yml` overlay with
  a Caddy bearer-auth sidecar in front of the streamable-http endpoint).
- **Auth roadmap, agreed order:** Bearer token first (done — see below), then
  API-Key, then OAuth2 client-credentials, then OIDC, then mTLS, then
  secret-rotation. Bearer was prioritized because the user's real target agent
  (a GoClaw / OpenClaw `a2a-gateway` instance) only supports `none` or `bearer`
  for inbound auth today — nothing else is currently relevant for that target.
- **Credentials are never persisted.** `registered_agents.json` holds only
  `url`/`name`/`description`. Bearer tokens live in `AppState.credentials`
  (in-memory only), supplied via `register_agent`'s optional `bearer_token`
  argument.

## Done (Phases 1–3, all pushed, all in PR #1)

| Phase | What | Verified how |
|---|---|---|
| 1 | Cargo workspace, deps (`rmcp`, `a2a`/`a2a-client` via git), `Bridge` MCP server skeleton (stdio + streamable-http), `AppState`, JSON persistence, Dockerfile, `docker-compose.yml`, `docker-compose.proxy.yml` + `proxy/` (Caddy bearer gate) | `cargo build/test/clippy` clean; real `initialize` + `tools/call` handshake over streamable-http via curl |
| 2 | `register_agent` / `list_agents` / `unregister_agent`, real `AgentCardResolver` fetch, persistence wired to every mutation + 5-min periodic save + save-on-exit | End-to-end against a real (if minimal) agent-card HTTP fixture, including error paths |
| 3 | `send_message` / `get_task_result` / `cancel_task` using real `A2AClient` calls; per-agent client cache (`AppState.agent_clients`) with card resolution + transport negotiation done once; bearer-token `AuthInterceptor` attached when a credential is on file | **Against the SDK's own reference agent** (`helloworld-server`, built from the vendored `a2a-rs` git checkout, not a hand-rolled fixture) — full round trip register → send → get-result, plus disk-state checks and error paths |

Tool list right now: `status`, `register_agent`, `list_agents`, `unregister_agent`,
`send_message`, `get_task_result`, `cancel_task`.

## Known gaps / open risks (read before continuing)

1. **`send_message` is unary and blocks until the agent reaches a terminal
   state.** There is no streaming yet (`send_message_stream`/`subscribe_to_task`),
   so a task that stays "working" (e.g. a real long-running agent call) will
   hang the tool call. `cancel_task`'s happy path (canceling something
   genuinely still running) has **not** been exercised for this reason — only
   its "unknown task_id" error path has. This is the main functional gap.
2. **Not tested against the user's real GoClaw instance yet.** Two concrete,
   verified-in-code compatibility risks to check there specifically:
   - GoClaw's `a2a-gateway` plugin badge says **A2A v0.3.0**; `a2a-rs` targets
     v1. Agent-card path (`/.well-known/agent-card.json`) and transports
     (JSON-RPC/REST) line up on paper, but this hasn't been confirmed against
     a live GoClaw instance.
   - `a2a-rs`'s `AgentCard.securitySchemes` deserializes a field-presence-keyed
     shape (e.g. `{"httpAuthSecurityScheme": {...}}`), confirmed by reading
     `a2a/src/agent_card.rs` directly — **not** the OpenAPI-style
     `{"type": "http", ...}`. If GoClaw's card JSON uses a different shape,
     the whole `AgentCard` deserialization fails and `register_agent` errors
     out. Worth an explicit first smoke test.
3. **Docker build itself is unverified.** No Docker daemon was available in
   the sandbox this was built in — `Dockerfile` was reasoned through carefully
   (aws-lc-rs's C/asm build needs `build-essential cmake perl` in the builder
   stage) but never actually run through `docker build`. Please confirm before
   relying on it.
4. **`proxy/Caddyfile` (bearer-auth gate) is unverified against a live Caddy.**
   Written against documented Caddy matcher/placeholder syntax, not run. If
   the user already has a proven `proxy/` folder from a sibling project
   (mentioned during planning), prefer that over this one.
5. **`A2A_SEED_AGENTS` is documented (`.env.example`, both compose files) but
   not yet read by the binary.** Only dynamic registration via the
   `register_agent` tool works today.
6. **The Python implementation is untouched** (`a2a_mcp_server.py`, `common/`,
   `persistence_utils.py`, `pyproject.toml`, `requirements.txt`,
   `config_creator.py`). Planned removal once the Rust port reaches parity —
   see "Not started" below. Don't delete it prematurely.

## Not started yet

- **Phase 4 — streaming** (`send_message_stream`, real `subscribe_to_task`
  usage): needed to fix gap #1 above.
- **Auth phases B–F** (API-Key, OAuth2, OIDC, mTLS, secret rotation) — only
  needed once an agent beyond GoClaw requires them.
- **`A2A_SEED_AGENTS` parsing** at startup.
- **Cleanup phase**: remove the Python implementation, update `README.md` and
  `smithery.yaml` for the Rust binary, decide the fate of `config_creator.py`
  (port vs. drop).
- **Real GoClaw smoke test** (see gap #2).

## Where to pick this back up

Read this file, then `git log --oneline` on this branch for the exact history,
then either continue with Phase 4 (streaming) or run the GoClaw smoke test
first — whichever the user prioritizes. The PR (#1) has the cumulative diff
and per-phase commit messages with more implementation detail than this file.

# Generative UI Suite

Agents publish **interactive HTML surfaces** — real Web Components, not screenshots — by
**writing files into a directory**. A Rust server **discovers** them (no submission API),
serves the **live version** of each surface, and writes user interactions (**backflow**)
back into the same directory, where the producing agent reads them. The filesystem is the
bus; discovery and backflow are symmetric.

It re-works the core idea of [`ui.plan.ai`](https://ui.plan.ai) — *make agent-generated UI
work inspectable* — but swaps pictures+click-zones for real HTML, the submission API for a
directory schema, and one-way events for a directory-based backflow loop.

## Concepts

- **agent** → **deck** → **surface** → **version**. A *deck* is a navigable set of
  interconnected surfaces (`main` by default). A *surface* is a regenerable "dashboard"
  whose newest version is live; history is retained.
- **backflow**: every interaction is written to the surface's `events/` directory.
- **stream**: live progress/status events (agent `emit_stream`, handler `host.emit`) appended
  to the surface's `stream/` directory and pushed over SSE (`/events`, `type: "stream"`); the
  latest value per name shows in the KV view as `stream.{name}` (bind with `data-kv`).

```
artifacts/{agent}/{deck}/{surface}/v/{version}/{frame.json,index.html}
artifacts/{agent}/{deck}/{surface}/events/{unix-ms}-{action}.json
artifacts/{agent}/{deck}/{surface}/stream/{unix-ms}-{name}.json   # append-only progress events
artifacts/{agent}/{deck}/deck.json     # optional: entry surface + nav order
```

## Quick start

Everything runs through the Nix flake (pinned Rust + node, cross-platform):

```sh
nix develop                                   # dev shell
cargo run -p gui-server -- --artifacts ./artifacts
# open http://localhost:8080/ for the inspector, or a surface directly:
# http://localhost:8080/demo/sales/
# http://localhost:8080/demo/handlers/   sandboxed JS/Lua/WASM handlers (click Bump → counts)
# http://localhost:8080/demo/main/progress/   streamed progress bound via data-kv
```

Surfaces can ship **sandboxed handlers** (JS/Lua/WASM) that run server-side on a backflow
action — they fold a per-surface KV view (bound in the UI via `data-kv`) and define the
response. Handlers run **by default** (default-deny capabilities + resource limits); pass
`--disable-handlers` to turn execution off.

Build a single binary without the shell:

```sh
nix build .#gui-server && ./result/bin/gui-server --artifacts ./artifacts
```

## Verify the loop end to end

```sh
nix develop                                   # pinned Rust + node dev shell
cargo test                                    # all crates
node --test suite/*.test.js                   # JS runtime helpers

# Serve the committed demo deck and open it:
cargo run -p gui-server -- --artifacts ./artifacts
#   http://localhost:8080/            inspector (live)
#   http://localhost:8080/demo/sales/ real interactive HTML; nav to region-detail/settings

# Drive the loop from an agent (in another shell, same artifacts dir):
cargo run -p example-agent -- --artifacts ./artifacts wait sales   # blocks
# …click "Refresh" in the browser → an event file lands in
#   artifacts/demo/main/sales/events/ and the agent prints it.

# Or without a browser, via the CLI:
cargo run -p gui-cli -- --artifacts ./artifacts backflow emit demo sales --action refresh
cargo run -p gui-cli -- --artifacts ./artifacts index
```

`gui kv-trigger` turns KV changes into [reagent](https://reagent.plan.ai) trigger events: one
JSON line (`{key, to, title, message, vars: {surface, kv_key, old, new, url, …}}`) per changed
key of a surface's public KV view. Use it as a `watch` trigger (or `poll` with `--once`); it
keeps its snapshot in `$REAGENT_STATE`. Filters: `--surface a/d/s`, `--key prefix`; `--to
running` messages the running task; `--with-stream` also reports `stream.*` progress.

The MCP server (`cargo run -p gui-mcp -- --artifacts ./artifacts`) exposes the same
authoring/awaiting flow to any MCP-speaking agent (`publish_surface`, `define_deck`,
`wait_for_backflow`, …). For speed, `scaffold_deck` publishes a whole linked deck of template
skeletons in one call, and `publish_surface` can render a template from slot data instead of
full HTML; agents keep their own templates in `--templates` (default `./templates`) via
`save_template`.

## Direct access — an agent that authors the whole app via the MCP

The `direct-access` example runs the HTTP server, the `gui-mcp` MCP server, and a
[swiftide](https://swiftide.rs) LLM agent **in one process** (separate libraries, wired
in-process over an in-memory pipe — no subprocesses). The agent connects to the MCP server,
loads the [`authoring-ui`](.agents/skills/authoring-ui/SKILL.md) skill as its system prompt,
and **builds the app itself** by calling the MCP tools (`publish_surface`, `define_deck`, …).

```sh
cargo run -p direct-access -- --artifacts ./run --mock        # deterministic, offline; keeps serving until Ctrl-C
# open http://127.0.0.1:8080/  — the surfaces the agent publishes appear live

# Real / OpenAI-compatible backend (opt-in; custom base URL supported):
OPENAI_API_KEY=… cargo run -p direct-access --features openai -- \
    --base-url https://your-endpoint/v1 --model gpt-5.4-mini \
    --goal "Build a signup screen, then a thank-you screen"
```

Notes:
- The default build uses a built-in mock LLM (no network) that drives one `publish_surface`
  call — what the test exercises. The real backend is gated behind `--features openai` (uses
  swiftide from git + `async-openai 0.33`; needs a recent cargo for git swiftide's 2024 edition).
- Pass a `--model` your endpoint hosts (query `GET {base-url}/models`). The agent is generic
  over any swiftide `ChatCompletion`, so the backend is pluggable.
- Verbose logging is on by default (stderr); tune with `RUST_LOG`.
- Verified live against a custom base URL: the agent published a real `<ui-*>` component tree
  through the MCP.

## Crates

| Crate | Role |
|---|---|
| `gui-schema` | The on-disk contract: manifests, decks, events, handlers, component tree → `<ui-*>` HTML. Generates `schemas/*.json`. |
| `gui-agent` | Agent API: publish surfaces (+ handlers), define decks, read/wait on backflow. |
| `gui-server` | Discover + serve surfaces, route backflow, run handlers, SSE, inspector. |
| `gui-runtime` | Sandboxed handler engines (JS/Lua/WASM) behind one capability-limited host API. |
| `gui-mcp` | `rmcp` stdio MCP server — `publish_surface`, `scaffold_deck`, templates, `define_deck`, `wait_for_backflow`, … |
| `gui-cli` | `gui` debug CLI — `index`, `validate`, `deck`, `surface`, `publish`, `backflow`, `watch`. |

See [`AGENTS.md`](./AGENTS.md) for the agent-facing reference and hard rules.

## License

MIT.

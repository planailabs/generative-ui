# Agent guide

Agent-facing entry point for the **Generative UI Suite**. Facts, tables, commands.

## What this is

Agents publish **interactive HTML surfaces** (real `<ui-*>` Web Components, not pictures)
by **writing files into a directory** that follows a fixed schema. A Rust server
**discovers** them (no submission API), serves the **live (newest) version** of each
surface, and writes user interactions — **backflow** — back into the same directory as
event files. The filesystem is the bus; discovery and backflow are symmetric.

## Vocabulary

| Term | Meaning |
|---|---|
| **agent** | A producer, identified by a globally unique slug. |
| **deck** | A navigable set of interconnected surfaces under an agent. `main` is the default. |
| **surface** | A stable, regenerable UI key (the "dashboard"). Newest version wins; history kept. |
| **version** | A published snapshot of a surface (`v/{unix-ms}/`). Sorts by id; max = live. |
| **backflow** | A user interaction written back into the surface's `events/` dir. |

## Directory schema (the contract)

```
artifacts/{agent}/{deck}/{surface}/v/{version}/{frame.json,index.html,assets/}
artifacts/{agent}/{deck}/{surface}/events/{unix-ms}-{action}.json   # server-written
artifacts/{agent}/{deck}/deck.json                                  # optional: entry, order
artifacts/{agent}/agent.json                                        # optional: name, description
```

The **path is the identity** — agent/deck/surface/version are not duplicated in manifests.

## Layout

| Path | What |
|---|---|
| `crates/gui-schema` | Shared types: manifest, deck, event, component tree → `<ui-*>` HTML. |
| `crates/gui-agent` | Agent API: publish surfaces, define decks, read/wait on backflow. |
| `crates/gui-server` | axum server: discover, serve, route backflow, SSE, inspector. |
| `crates/gui-mcp` | `rmcp` stdio MCP server exposing the agent API as tools. |
| `crates/gui-cli` | `gui` debug CLI: index/validate/deck/surface/publish/backflow/watch. |
| `suite/` | Web Components runtime (native ESM, no build) + CSS. |
| `schemas/` | Published JSON Schema for the on-disk contract. |
| `artifacts/` | Example deck of surfaces. |
| `examples/` | Example Rust agent (publish + wait for backflow). |

## Commands (run inside the Nix dev shell)

```bash
nix develop                 # enter the dev shell (pinned Rust + node)
cargo build                 # build the workspace
cargo test                  # run all tests (the gate)
cargo run -p gui-server -- --artifacts ./artifacts
cargo run -p gui-cli -- index ./artifacts
nix flake check             # cargo test + clippy as flake checks
nix build .#gui-server      # build a single binary
```

## Skills

- [`.agents/skills/authoring-ui`](.agents/skills/authoring-ui/SKILL.md) — how an agent uses
  the `gui-mcp` tools to show users interactive UI and react to their input.
- [`.agents/skills/browser-verify`](.agents/skills/browser-verify/SKILL.md) — verify surfaces
  render correctly in headless chromium after changing the suite runtime.

## Hard rules

- **No submission API.** Producers create artifacts by writing files in the schema above.
- **Latest-wins, append-only.** Publishing drops a newer `v/{version}/`; never mutate old versions.
- **Atomic writes.** Write to a temp file + `rename` so concurrent readers never see partials.
- **Cross-OS.** Build paths with `Path::join`; no hardcoded separators or `/tmp`.
- **Native ESM only** in the suite. No bundler, no transpile, no CommonJS.
- **Commit closes every stage** once it builds and tests pass (Conventional Commits).

## Keep in sync (update tasks)

These things are coupled — when you change one, update the others in the **same** change:

- **Add/modify a `<ui-*>` component** → update all of: `crates/gui-schema` `Component` enum +
  HTML renderer; `suite/runtime.js` (the custom element **and** the plain-HTML→component
  rewrite map); `suite/suite.css` (styling); `crates/gui-mcp/src/component_help.txt`;
  `.agents/skills/authoring-ui/SKILL.md`; and **`artifacts/`** (showcase it in the demo deck).
- **Update `artifacts/`** whenever components, the suite, or the manifest schema change, so the
  committed demo deck stays valid and representative. Then browser-verify it.
- **Edit `suite/` (runtime/CSS)** → rebuild `gui-server` (the suite is embedded via
  `include_str!`) and **browser-verify** (Rust tests don't render). The plain-HTML rewrite must
  run *before* `customElements.define`.
- **Change the directory schema** (manifest/event/deck/path) → update `schemas/*.schema.json`,
  `gui-schema` types + discovery, and the schema/vocabulary in this file and `README.md`.
- **Change routes** → update the route lists in `README.md` and any references here.
- **Change an MCP tool** (`gui-mcp`) → update `component_help.txt`, the `authoring-ui` skill,
  the `gui-mcp` tests, and `examples/direct-access`'s `MockLlm`.
- **Add a binary or crate** → update `flake.nix` (`cargoBuildFlags`, `packages`, `apps`), the
  Layout table here, and the crates table in `README.md`.
- **Bump the `direct-access` swiftide git rev** → recompute and update the `outputHashes` entry
  in `flake.nix` (`nix-prefetch-git --url … --rev …`).

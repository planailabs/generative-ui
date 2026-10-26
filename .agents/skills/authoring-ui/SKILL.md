---
name: authoring-ui
description: Author interactive UI surfaces and react to user input using the gui-mcp MCP server (publish_surface, scaffold_deck, templates, define_deck, wait_for_backflow, component_help). Use when an agent needs to show a user real interactive HTML and act on what they do.
---

# Authoring interactive UI via the MCP

The `gui-mcp` server lets you **show users real interactive HTML** (not pictures) and
**react to what they do**. You publish a *surface*; the server renders it as live Web
Components; the user interacts; the interaction flows back to you as a *backflow* event.
Everything goes through a directory — there is no submission API and no callback URL.

Connect `gui-mcp` (stdio) pointed at an artifacts directory, then drive these tools.

## Vocabulary

- **surface** — a named, regenerable screen (the "dashboard"). Newest version is live.
- **deck** — a navigable set of interconnected surfaces (`main` is the default).
- **backflow** — a user interaction (button click / form submit) returned to you.

## The core loop

1. `component_help` — read the component reference once (cache it).
2. `publish_surface` — create or regenerate a surface from a component tree.
3. `wait_for_backflow` — block until the user interacts; you get `{ action, value }`.
4. React: `publish_surface` again (latest-wins) to update the screen, then wait again.

Repeat 3–4 for a conversation conducted entirely through the UI.

## publish_surface

```json
{
  "agent": "assistant",
  "deck": "main",
  "surface": "ask-name",
  "title": "What should I call you?",
  "alt_text": "A form asking for the user's name",
  "html": "<ui-heading level=\"1\">What should I call you?</ui-heading><ui-form data-action=\"submit_name\"><ui-input name=\"name\" label=\"Your name\" placeholder=\"Ada\"></ui-input></ui-form>"
}
```

- Provide `html`: the surface body written with the `<ui-*>` components — call
  `component_help` for the full element reference.
- You may also write ordinary HTML (`<h1>`, `<p>`, `<button>`, `<form>`, `<select>`,
  `<a data-to="slug">`); it is **live-rewritten** into the matching `<ui-*>` components in the
  browser. Prefer the `<ui-*>` components directly; the plain-HTML translation is a convenience.
- `nav: [{ "to": "other-surface", "label": "..." }]` links surfaces into a navigable deck.
- Republishing the same `surface` supersedes the old version (history is kept).

## Fast path: templates and scaffold_deck

Writing every surface as full HTML is the slow part. Instead:

- `scaffold_deck` — one call publishes a whole deck of linked surfaces and wires `deck.json`
  (order as given, entry = first). Each surface renders from a template (default `skeleton`:
  title, purpose, placeholder `sections`, links), so the user can click through immediately:
  ```json
  { "agent": "assistant", "deck": "crm", "surfaces": [
    { "surface": "contacts", "title": "Contacts", "purpose": "Browse people",
      "links": [{ "to": "add-contact", "label": "Add" }] },
    { "surface": "add-contact", "title": "Add contact", "template": "form",
      "data_json": "{\"action\":\"add\",\"fields\":[{\"name\":\"email\",\"type\":\"email\"}]}" } ] }
  ```
  Then fill surfaces one by one (or in parallel) with `publish_surface`; the newer version
  replaces the skeleton.
- `publish_surface` with `template` + `data_json` (a JSON object as a string) instead of
  `html`: you send slot values, the server renders the markup. `title` and `nav` fill in
  from the call. `list_templates` shows each template's slots (built-ins: `skeleton`, `form`,
  `list`, `dashboard`, partial `nav`).
- **Your own templates**: `save_template` stores a minijinja (Jinja2) HTML template in the
  templates dir (`gui-mcp --templates`, default `./templates`); you may also write
  `{name}.html` files there directly. Start it with a `{# description + slots #}` comment.
  Templates `{% include "nav" %}` / `{% extends "card" %}` each other by name; values are
  HTML-escaped (use `| tojson` inside `data-value='…'`). A saved template with a built-in's
  name shadows it. `read_template` returns any template's source to adapt.

## wait_for_backflow

```json
{ "agent": "assistant", "surface": "ask-name", "timeout_ms": 120000 }
```

Returns `{ "received": true, "event": { "action": "submit_name", "value": { "name": "Ada" }, ... } }`
or `{ "received": false }` on timeout.

- A **button** yields `{ action, value }` (its `data-action` + `data-value`).
- A **form** yields `{ action: "<form action>", value: { field: value, ... } }`.

## Handlers (sandboxed server-side logic)

Instead of waiting for backflow and republishing, you can attach **handlers** to a surface:
sandboxed code (JS, Lua, or WASM) that runs on the server when a backflow action arrives. A
handler **never edits the surface** — it folds the surface's **KV view** (a small key/value
store the UI reads) and defines the HTTP response. Bind a KV value in your HTML with
`data-kv="key"`; the browser refreshes those nodes after each interaction, with no reload.
(Handler execution is on by default; the server's `--disable-handlers` turns it off.)

```json
{
  "agent": "assistant", "surface": "counter",
  "title": "Counter", "alt_text": "A click counter",
  "html": "<ui-button data-action=\"bump\">Bump</ui-button> <span data-kv=\"hits\">0</span>",
  "handlers": [{
    "action": "bump", "lang": "js", "kv": true,
    "source": "function handle(event, host) { host.kv.set('hits', (host.kv.get('hits')||0)+1); return { hits: host.kv.get('hits') }; }"
  }]
}
```

Capabilities are **default-deny**: grant `kv`, `fs`, and `http_allow: ["host"]` only as needed.
The unified `host` API (identical in js/lua/wasm) is in `component_help`: `host.kv.*`,
`host.http.fetch`, `host.fs.*` (jailed), `host.log`, `host.response`/`host.responseCode`, and
`host.crypto.*` (ungated — hash/hmac/aead/random/encode/hkdf/pbkdf2 + ed25519/ecdsa/rsa/x25519;
no capability needed).
Use handlers for data/side-effects (counters, fetches, caching); keep surface *layout* changes
as `publish_surface`. WASM handlers pass the module bytes via `source_base64`.

**Events & visibility.** A handler calls `host.emit(name, data)` to record an event you pick up
with `wait_for_backflow`/`list_backflow` — the same stream a direct UI submission produces (a
button/form's `data-action` is the event name, its fields the data), so handlers are optional.
Once processed, clean up: `drain_backflow` deletes a surface's whole event log;
`delete_backflow` deletes selectively by `file` (the id from `list_backflow`), exact `action`,
or a `since`/`until` millis range.
Public (`host.kv`/`host.emit`) is visible to the browser (bound via `data-kv`, served on the
events/kv APIs); private (`host.kvPrivate`/`host.emitPrivate`) is agent-only. You always see both.

## Patterns

- **Prompt the user**: publish a surface with an input/form, `wait_for_backflow`, read `value`.
- **Branch**: inspect the event `action`/`value`, then publish the next surface.
- **Multi-step flow**: wire surfaces with `nav` + `define_deck` (entry + order) so the user
  can move between them; wait on whichever surface is active.
- **Long-lived dashboard**: republish the surface as data changes; the user's browser
  re-renders (streamable views).

## Rules

- Always `wait_for_backflow` after publishing something interactive — don't assume input.
- Keep `alt_text` meaningful (accessibility).
- Slugs are lowercase `[a-z0-9-]`. The path `agent/deck/surface` is the identity.
- Don't hand-craft file paths; the tools write the correct directory shape for you.

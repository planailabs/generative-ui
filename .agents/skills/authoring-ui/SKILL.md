---
name: authoring-ui
description: Author interactive UI surfaces and react to user input using the gui-mcp MCP server (publish_surface, define_deck, wait_for_backflow, component_help). Use when an agent needs to show a user real interactive HTML and act on what they do.
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
  "components": [
    { "type": "heading", "text": "What should I call you?" },
    { "type": "form", "action": "submit_name", "children": [
      { "type": "input", "name": "name", "label": "Your name", "placeholder": "Ada" }
    ]}
  ]
}
```

- Provide `components` (a tree — call `component_help` for every node type) **or** `html`.
- `nav: [{ "to": "other-surface", "label": "..." }]` links surfaces into a navigable deck.
- Republishing the same `surface` supersedes the old version (history is kept).

## wait_for_backflow

```json
{ "agent": "assistant", "surface": "ask-name", "timeout_ms": 120000 }
```

Returns `{ "received": true, "event": { "action": "submit_name", "value": { "name": "Ada" }, ... } }`
or `{ "received": false }` on timeout.

- A **button** yields `{ action, value }` (its `data-action` + `data-value`).
- A **form** yields `{ action: "<form action>", value: { field: value, ... } }`.

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

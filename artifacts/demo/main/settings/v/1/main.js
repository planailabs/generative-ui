// `save` handler: the form submits { threshold, email }. Persist to KV (so the confirmation
// below reflects what's stored) and emit a `settings-saved` event the agent can pick up.
function handle(event, host) {
  const v = event.value || {};
  host.kv.set("threshold", v.threshold || "(none)");
  host.kv.set("email", v.email ? "on" : "off");
  host.kv.set("saved_at", event.received_at);
  host.emit("settings-saved", v);
  return { saved: true };
}

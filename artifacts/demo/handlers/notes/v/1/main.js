// Handler for the `save` form action: persist the note to the jailed state/ dir, mirror it
// into KV (so the UI's data-kv="last" binding shows it), and acknowledge.
function handle(event, host) {
  const note = (event.value && event.value.note) || "";
  host.fs.write("last.txt", note);
  host.kv.set("last", host.fs.read("last.txt")); // public: bound in the UI via data-kv
  host.emit("note-saved", { note }); // notify the agent (a public event on the events API)
  return { saved: true, last: host.kv.get("last") };
}

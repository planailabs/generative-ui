-- Sandboxed Lua (Luau) handler for the `bump` action. Returns a function(event, host); folds
-- a counter into KV and returns it. Same unified host API as the JS/WASM handlers.
return function(event, host)
  host.kv.set("hits", (host.kv.get("hits") or 0) + 1)
  return { hits = host.kv.get("hits") }
end

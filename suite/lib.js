// Pure helpers for the suite — no DOM access, so they import cleanly under `node --test`.

/** Build the JSON body the browser POSTs to the backflow endpoint. */
export function backflowPayload(action, component, value, version, timestamp) {
  return {
    action,
    component: component ?? null,
    value: value ?? null,
    version: version ?? null,
    timestamp,
  };
}

/** Canonical live URL for a sibling surface in the same deck. */
export function surfaceUrl(agent, deck, surface) {
  return `/${agent}/${deck}/${surface}/`;
}

/** The backflow endpoint for a surface id of the form `agent/deck/surface`. */
export function backflowUrl(surfaceId) {
  return `/backflow/${surfaceId}`;
}

/** Previous/next surface slugs for deck navigation. */
export function navTargets(order, current) {
  const list = Array.isArray(order) ? order : [];
  const i = list.indexOf(current);
  return {
    prev: i > 0 ? list[i - 1] : null,
    next: i >= 0 && i < list.length - 1 ? list[i + 1] : null,
  };
}

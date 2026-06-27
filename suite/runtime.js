// Generative UI Suite runtime — native ES module, no build step.
//
// Registers the <ui-*> custom elements, wires interactions back to the server (backflow),
// and resolves <ui-link>/<ui-deck-nav> navigation. The artifact's HTML is real, interactive
// DOM — not a picture.

import { backflowPayload, backflowUrl, navTargets, surfaceUrl } from "./lib.js";

// --- surface context, injected by the server into the document head ---
const ctx = (() => {
  const meta = (n) => document.querySelector(`meta[name="${n}"]`)?.content || "";
  const surface = meta("surface");
  const version = meta("version");
  let deck = {};
  const el = document.getElementById("gui-deck");
  if (el) {
    try {
      deck = JSON.parse(el.textContent);
    } catch {
      /* malformed deck graph — navigation degrades, surface still renders */
    }
  }
  const [agent = "", deckSlug = "", surfaceSlug = ""] = surface.split("/");
  return { surface, version, deck, agent, deckSlug, surfaceSlug };
})();

/** POST an interaction back into the surface's directory via the server. */
async function sendBackflow(action, component, value) {
  if (!ctx.surface) return;
  const body = backflowPayload(action, component, value, ctx.version || null, new Date().toISOString());
  try {
    await fetch(backflowUrl(ctx.surface), {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
    });
  } catch (e) {
    console.error("backflow failed", e);
  }
}

const sibling = (slug) => surfaceUrl(ctx.agent, ctx.deckSlug, slug);

class UiButton extends HTMLElement {
  connectedCallback() {
    if (this._wired) return;
    this._wired = true;
    const label = this.textContent.trim();
    this.textContent = "";
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "ui-btn";
    btn.textContent = label;
    btn.addEventListener("click", () => {
      const action = this.getAttribute("data-action");
      let value = null;
      const raw = this.getAttribute("data-value");
      if (raw != null) {
        try {
          value = JSON.parse(raw);
        } catch {
          value = raw;
        }
      }
      this.dispatchEvent(new CustomEvent("ui-action", { bubbles: true, detail: { action, value } }));
      if (action) sendBackflow(action, "ui-button", value);
    });
    this.appendChild(btn);
  }
}

function field(labelText) {
  const wrap = document.createElement("label");
  wrap.className = "ui-field";
  if (labelText) {
    const s = document.createElement("span");
    s.className = "ui-label";
    s.textContent = labelText;
    wrap.appendChild(s);
  }
  return wrap;
}

class UiInput extends HTMLElement {
  connectedCallback() {
    if (this._wired) return;
    this._wired = true;
    const name = this.getAttribute("name") || "";
    const wrap = field(this.getAttribute("label"));
    const input = document.createElement("input");
    input.name = name;
    input.type = this.getAttribute("type") || "text";
    input.placeholder = this.getAttribute("placeholder") || "";
    input.className = "ui-control";
    this.innerHTML = "";
    wrap.appendChild(input);
    this.appendChild(wrap);
  }
}

class UiSelect extends HTMLElement {
  connectedCallback() {
    if (this._wired) return;
    this._wired = true;
    const name = this.getAttribute("name") || "";
    const options = [...this.querySelectorAll("option")];
    const wrap = field(this.getAttribute("label"));
    const select = document.createElement("select");
    select.name = name;
    select.className = "ui-control";
    options.forEach((o) => select.appendChild(o));
    this.innerHTML = "";
    wrap.appendChild(select);
    this.appendChild(wrap);
  }
}

class UiCheckbox extends HTMLElement {
  connectedCallback() {
    if (this._wired) return;
    this._wired = true;
    const name = this.getAttribute("name") || "";
    const checked = this.hasAttribute("checked");
    const text = this.textContent.trim();
    this.innerHTML = "";
    const wrap = document.createElement("label");
    wrap.className = "ui-check";
    const input = document.createElement("input");
    input.type = "checkbox";
    input.name = name;
    input.checked = checked;
    const span = document.createElement("span");
    span.textContent = text;
    wrap.append(input, span);
    this.appendChild(wrap);
  }
}

class UiForm extends HTMLElement {
  connectedCallback() {
    if (this._wired) return;
    this._wired = true;
    const action = this.getAttribute("data-action") || "";
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "ui-btn ui-submit";
    btn.textContent = this.getAttribute("submit-label") || "Submit";
    btn.addEventListener("click", () => {
      const values = this.collect();
      this.dispatchEvent(new CustomEvent("ui-submit", { bubbles: true, detail: { action, values } }));
      sendBackflow(action, "ui-form", values);
    });
    this.appendChild(btn);
  }

  collect() {
    const out = {};
    this.querySelectorAll("input[name], select[name], textarea[name]").forEach((el) => {
      out[el.name] = el.type === "checkbox" ? el.checked : el.value;
    });
    return out;
  }
}

class UiLink extends HTMLElement {
  connectedCallback() {
    if (this._wired) return;
    this._wired = true;
    const to = this.getAttribute("to");
    const label = this.textContent.trim() || to || "";
    this.textContent = "";
    const a = document.createElement("a");
    a.className = "ui-link";
    a.href = to ? sibling(to) : "#";
    a.textContent = label;
    this.appendChild(a);
  }
}

class UiDeckNav extends HTMLElement {
  connectedCallback() {
    if (this._wired) return;
    this._wired = true;
    const order = ctx.deck.order || [];
    const { prev, next } = navTargets(order, ctx.surfaceSlug);
    const nav = document.createElement("nav");
    nav.className = "ui-deck-nav";

    const link = (slug, text, cls) => {
      const a = document.createElement("a");
      a.className = "ui-deck-link" + (cls ? " " + cls : "");
      a.href = sibling(slug);
      a.textContent = text;
      return a;
    };

    if (prev) nav.appendChild(link(prev, "← " + prev, "prev"));
    const mid = document.createElement("span");
    mid.className = "ui-deck-list";
    order.forEach((slug) => mid.appendChild(link(slug, slug, slug === ctx.surfaceSlug ? "current" : "")));
    nav.appendChild(mid);
    if (next) nav.appendChild(link(next, next + " →", "next"));

    this.appendChild(nav);
  }
}

class UiHeading extends HTMLElement {
  connectedCallback() {
    this.setAttribute("role", "heading");
    this.setAttribute("aria-level", this.getAttribute("level") || "2");
  }
}

customElements.define("ui-button", UiButton);
customElements.define("ui-input", UiInput);
customElements.define("ui-select", UiSelect);
customElements.define("ui-checkbox", UiCheckbox);
customElements.define("ui-form", UiForm);
customElements.define("ui-link", UiLink);
customElements.define("ui-deck-nav", UiDeckNav);
customElements.define("ui-heading", UiHeading);

export { ctx, sendBackflow };

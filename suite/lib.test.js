// Pure-helper tests for the suite runtime. Run with: node --test suite/
// No npm install — uses the built-in node:test runner and node:assert.

import { test } from "node:test";
import assert from "node:assert/strict";

import { backflowPayload, surfaceUrl, backflowUrl, navTargets } from "./lib.js";

test("backflowPayload normalizes nullables", () => {
  assert.deepEqual(backflowPayload("buy", "ui-button", { id: 1 }, "100", "t"), {
    action: "buy",
    component: "ui-button",
    value: { id: 1 },
    version: "100",
    timestamp: "t",
  });
  assert.deepEqual(backflowPayload("x", undefined, undefined, undefined, "t"), {
    action: "x",
    component: null,
    value: null,
    version: null,
    timestamp: "t",
  });
});

test("surfaceUrl / backflowUrl", () => {
  assert.equal(surfaceUrl("demo", "main", "sales"), "/demo/main/sales/");
  assert.equal(backflowUrl("demo/main/sales"), "/backflow/demo/main/sales");
});

test("navTargets resolves prev/next", () => {
  assert.deepEqual(navTargets(["a", "b", "c"], "b"), { prev: "a", next: "c" });
  assert.deepEqual(navTargets(["a", "b", "c"], "a"), { prev: null, next: "b" });
  assert.deepEqual(navTargets(["a", "b", "c"], "c"), { prev: "b", next: null });
  assert.deepEqual(navTargets([], "x"), { prev: null, next: null });
  assert.deepEqual(navTargets(["a"], "missing"), { prev: null, next: null });
});

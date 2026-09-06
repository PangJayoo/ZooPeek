import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const appSource = readFileSync(new URL("./App.vue", import.meta.url), "utf8");

const instanceRegistries = [
  "treeInstRefs",
  "scrollbarRefs",
  "searchInputRefs",
  "treePaneRefs",
  "spotlightRefs",
  "shellScrollRefs",
  "shellInputRefs",
];

test("template ref registries are non-reactive", () => {
  for (const name of instanceRegistries) {
    assert.doesNotMatch(
      appSource,
      new RegExp(`const\\s+${name}\\s*=\\s*ref`),
      `${name} must not trigger renders while Vue assigns template refs`
    );
  }
});

test("tree lazy loader is installed only after connection sync", async () => {
  const storeModule = await import("./store.ts");
  const canLoadTree = (storeModule as Record<string, unknown>).canLoadTree;
  assert.equal(typeof canLoadTree, "function");
  const predicate = canLoadTree as (status: string) => boolean;
  assert.equal(predicate("Connecting"), false);
  assert.equal(predicate("Disconnected"), false);
  assert.equal(predicate("SyncConnected"), true);
  assert.equal(predicate("ConnectedReadOnly"), true);
  assert.match(
    appSource,
    /:on-load="canLoadTree\(tab\.status\)\s*\?[^\"]+:\s*undefined"/
  );
});

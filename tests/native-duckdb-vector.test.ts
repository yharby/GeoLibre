import assert from "node:assert/strict";
import { before, test } from "node:test";

// Mock the Tauri invoke boundary.
const calls: Array<{ cmd: string; args: unknown }> = [];
let responder: (cmd: string, args: any) => unknown = () => ({});

const coreMock = {
  invoke: async (cmd: string, args: any) => {
    calls.push({ cmd, args });
    return responder(cmd, args);
  },
};

// Wire the module under test to the mock via a loader hook.
// The seam must be installed before the module is imported so the lazy
// function-level check picks it up on first call.
(globalThis as any).__GEOLIBRE_INVOKE__ = coreMock.invoke;

type NativeDuckDbVectorModule =
  typeof import("../apps/geolibre-desktop/src/lib/native-duckdb-vector");
let loadNativeVectorFile: NativeDuckDbVectorModule["loadNativeVectorFile"];

before(async () => {
  ({ loadNativeVectorFile } = await import(
    "../apps/geolibre-desktop/src/lib/native-duckdb-vector.ts"
  ));
});

test("returns the parsed FeatureCollection on a direct load", async () => {
  calls.length = 0;
  responder = () => ({
    needsConfirmation: false,
    featureCollection: JSON.stringify({
      type: "FeatureCollection",
      features: [{ type: "Feature", geometry: null, properties: { id: 1 } }],
    }),
  });
  const fc = await loadNativeVectorFile("/abs/a.parquet", "parquet");
  assert.equal(fc.type, "FeatureCollection");
  assert.equal(fc.features.length, 1);
  assert.equal(calls[0].cmd, "load_native_vector");
});

test("prompts and re-invokes confirmed when needsConfirmation", async () => {
  calls.length = 0;
  let firstCall = true;
  responder = () => {
    if (firstCall) {
      firstCall = false;
      return { needsConfirmation: true, featureCount: 600000 };
    }
    return {
      needsConfirmation: false,
      featureCollection: JSON.stringify({ type: "FeatureCollection", features: [] }),
    };
  };
  const fc = await loadNativeVectorFile("/abs/big.parquet", "parquet", {
    onLargeDataset: () => true,
  });
  assert.equal(fc.features.length, 0);
  assert.equal(calls.length, 2);
  assert.equal((calls[1].args as any).options.largeDatasetConfirmed, true);
});

import assert from "node:assert/strict";
import { before, beforeEach, test } from "node:test";
import type { FeatureCollection } from "geojson";

// ─── Seam: isTauri() + Tauri invoke ───────────────────────────────────────────
// isTauri() checks `"__TAURI_INTERNALS__" in window`.
// @tauri-apps/api/core's invoke() delegates to window.__TAURI_INTERNALS__.invoke.
// readLocalFileBytes in tauri-io.ts also falls back through the same invoke().
// All Tauri IPC calls are routed through one mutable `invokeResponder`.
let invokeResponder: (cmd: string, args: unknown) => unknown = () => {
  throw new Error("No invoke responder set");
};

// window.__TAURI_INTERNALS__.invoke must remain a FUNCTION for the lifetime of
// the test suite — tests must never replace the whole __TAURI_INTERNALS__ object
// with a plain `{}` because @tauri-apps/api/core reads .invoke at call time.
const fakeWindow: Record<string, unknown> = {
  location: { href: "http://localhost" },
  __TAURI_INTERNALS__: {
    invoke: (cmd: string, args: unknown) => invokeResponder(cmd, args),
  },
};
(globalThis as Record<string, unknown>).window = fakeWindow;

// ─── Seam: native-duckdb-vector invoke ────────────────────────────────────────
// native-duckdb-vector.ts uses globalThis.__GEOLIBRE_INVOKE__ as a seam;
// route it through the same invokeResponder.
(globalThis as Record<string, unknown>).__GEOLIBRE_INVOKE__ = (
  cmd: string,
  args: unknown,
) => invokeResponder(cmd, args);

// ─── Seam: loadDuckDbVector ───────────────────────────────────────────────────
// tauri-io.ts checks globalThis.__GEOLIBRE_DUCKDB_VECTOR__ before importing the
// DuckDB-WASM bundle (which has Vite ?url imports that fail under Node).
import type { DuckDbVectorFile } from "../apps/geolibre-desktop/src/lib/duckdb-vector-loader";
import type { DuckDbVectorLoadOptions } from "../apps/geolibre-desktop/src/lib/duckdb-vector-guard";

let wasmCallCount = 0;
let wasmResponder: (
  file: DuckDbVectorFile,
  options?: DuckDbVectorLoadOptions,
) => Promise<FeatureCollection> = async () => {
  throw new Error("No WASM responder set");
};

(globalThis as Record<string, unknown>).__GEOLIBRE_DUCKDB_VECTOR__ = (
  file: DuckDbVectorFile,
  options?: DuckDbVectorLoadOptions,
): Promise<FeatureCollection> => {
  wasmCallCount++;
  return wasmResponder(file, options);
};

// ─── Module under test ────────────────────────────────────────────────────────
// All seams must be installed before tauri-io is imported.
(globalThis as Record<string, unknown>).self ??= globalThis;

let loadDroppedVectorPaths: (
  paths: string[],
  options?: DuckDbVectorLoadOptions,
) => Promise<Array<{ data: FeatureCollection; path: string }>>;

before(async () => {
  ({ loadDroppedVectorPaths } = await import(
    "../apps/geolibre-desktop/src/lib/tauri-io.ts"
  ) as any);
});

// ─── Test fixtures ────────────────────────────────────────────────────────────

const NATIVE_FC: FeatureCollection = {
  type: "FeatureCollection",
  features: [{ type: "Feature", geometry: null, properties: { src: "native" } }],
};

const WASM_FC: FeatureCollection = {
  type: "FeatureCollection",
  features: [{ type: "Feature", geometry: null, properties: { src: "wasm" } }],
};

// ─── Invoke responder factories ───────────────────────────────────────────────

/** Respond to load_native_vector with a successful FeatureCollection. */
function setNativeSuccess() {
  invokeResponder = (cmd) => {
    if (cmd === "load_native_vector") {
      return {
        needsConfirmation: false,
        featureCollection: JSON.stringify(NATIVE_FC),
      };
    }
    throw new Error(`Unexpected invoke in native-success: ${String(cmd)}`);
  };
}

/**
 * Fail load_native_vector with a generic error; the WASM path is then
 * exercised. readLocalFileBytes attempts:
 *   1. readFile() -> calls window.__TAURI_INTERNALS__.invoke("plugin:fs|read_file")
 *      -> we throw a scope error to exercise the Tauri fallback.
 *   2. invoke("read_local_file") -> we return an empty ArrayBuffer.
 * loadDuckDbVector is intercepted by the __GEOLIBRE_DUCKDB_VECTOR__ seam.
 */
function setNativeFailure() {
  invokeResponder = (cmd) => {
    if (cmd === "load_native_vector") {
      throw new Error("native engine failed");
    }
    // readLocalFileBytes: fs plugin fallback
    if (cmd === "plugin:fs|read_file") {
      throw new Error("fs scope denied");
    }
    // readLocalFileBytes: Tauri command fallback
    if (cmd === "read_local_file") {
      return new ArrayBuffer(0);
    }
    throw new Error(`Unexpected invoke in native-failure: ${String(cmd)}`);
  };
}

// ─── Per-test reset ───────────────────────────────────────────────────────────

beforeEach(() => {
  wasmCallCount = 0;
  wasmResponder = async () => WASM_FC;
});

// ─── Tests ────────────────────────────────────────────────────────────────────

test("native success short-circuits WASM", async () => {
  // Arrange: Tauri desktop with a successful native load.
  setNativeSuccess();

  // Act: use a .parquet extension so only the DuckDB branch is reached
  // (GeoJSON / KMZ / KML / GPX / CSV branches all exit before the native path).
  const layers = await loadDroppedVectorPaths(["/data/test.parquet"]);

  // Assert: native result is returned and the WASM loader is never invoked.
  assert.equal(layers.length, 1, "one layer should be returned");
  assert.deepEqual(layers[0].data, NATIVE_FC, "layer data should be the native FC");
  assert.equal(wasmCallCount, 0, "WASM loader must NOT be called on native success");
});

test("native generic error falls back to WASM", async () => {
  // Arrange: Tauri desktop with a native load that throws a plain Error.
  setNativeFailure();

  // Act.
  const layers = await loadDroppedVectorPaths(["/data/test.parquet"]);

  // Assert: WASM fallback ran and its result was returned.
  assert.equal(layers.length, 1, "one layer should be returned from WASM fallback");
  assert.deepEqual(layers[0].data, WASM_FC, "layer data should be the WASM FC");
  assert.equal(wasmCallCount, 1, "WASM loader must be called exactly once on native failure");
});

test("native cancellation propagates without WASM retry", async () => {
  // Arrange: Tauri desktop with a native load that is cancelled by the user.
  const { VectorLoadCancelledError } = await import(
    "../apps/geolibre-desktop/src/lib/duckdb-vector-guard.ts"
  );
  invokeResponder = (cmd) => {
    if (cmd === "load_native_vector") {
      throw new VectorLoadCancelledError("user declined");
    }
    throw new Error(`Unexpected invoke in cancellation: ${String(cmd)}`);
  };

  // Act: loadDroppedVectorPaths swallows VectorLoadCancelledError (per its
  // contract it skips cancelled files without rethrowing), so we check that
  // zero layers were returned and the WASM loader was never called.
  const layers = await loadDroppedVectorPaths(["/data/test.parquet"]);

  assert.equal(layers.length, 0, "cancelled file must be skipped (no layers)");
  assert.equal(wasmCallCount, 0, "WASM loader must NOT be called after cancellation");
});

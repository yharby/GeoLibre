import type { FeatureCollection } from "geojson";
import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import {
  confirmLargeDataset,
  DUCKDB_VECTOR_FEATURE_WARN_COUNT,
  type DuckDbVectorLoadOptions,
} from "./duckdb-vector-guard";

interface NativeVectorResult {
  needsConfirmation: boolean;
  featureCount?: number;
  featureCollection?: string;
}

// Test seam: tests set globalThis.__GEOLIBRE_INVOKE__ to avoid bundling Tauri.
function invoke<T>(cmd: string, args: Record<string, unknown>): Promise<T> {
  const override = (globalThis as { __GEOLIBRE_INVOKE__?: typeof tauriInvoke })
    .__GEOLIBRE_INVOKE__;
  return (override ?? tauriInvoke)<T>(cmd, args);
}

function toFeatureCollection(json: string): FeatureCollection {
  return JSON.parse(json) as FeatureCollection;
}

export async function loadNativeVectorFile(
  path: string,
  extension: string,
  options: DuckDbVectorLoadOptions = {},
): Promise<FeatureCollection> {
  const baseOptions = {
    layer: options.layer,
    overrideSourceCrs: options.overrideSourceCrs,
    featureWarnCount: DUCKDB_VECTOR_FEATURE_WARN_COUNT,
    // Pre-confirm when there is no callback to prompt with.  That way the Rust
    // side skips the feature count entirely and reads the file in a single
    // pass, mirroring the WASM loader's "only gate when a callback is
    // attached" behaviour.  When a callback IS provided, leave unconfirmed so
    // Rust counts and may return needsConfirmation for the prompt flow below.
    largeDatasetConfirmed: !options.onLargeDataset,
  };

  const first = await invoke<NativeVectorResult>("load_native_vector", {
    path,
    extension,
    options: baseOptions,
  });

  if (first.needsConfirmation) {
    // Reuse the same confirmation UX the WASM loader uses; throws
    // VectorLoadCancelledError if the user declines.
    await confirmLargeDataset(
      { name: path, featureCount: first.featureCount ?? 0 },
      options.onLargeDataset,
    );
    const confirmed = await invoke<NativeVectorResult>("load_native_vector", {
      path,
      extension,
      options: { ...baseOptions, largeDatasetConfirmed: true },
    });
    if (!confirmed.featureCollection) {
      throw new Error("Native loader returned no GeoJSON after confirmation.");
    }
    return toFeatureCollection(confirmed.featureCollection);
  }

  if (!first.featureCollection) {
    throw new Error("Native loader returned no GeoJSON.");
  }
  return toFeatureCollection(first.featureCollection);
}

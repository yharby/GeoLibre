import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { DEFAULT_LAYER_STYLE, type GeoLibreLayer } from "@geolibre/core";
import { syncLayer } from "../packages/map/src/layer-sync";

// Stateful fake MapLibre map (mirrors label-sync.test.ts) so a test can read the
// source spec syncGeoJsonLayer passes to addSource.
function makeMap() {
  const sources = new Map<string, Record<string, unknown>>();
  const layers = new Map<string, Record<string, unknown>>();
  const map = {
    getSource: (id: string) =>
      sources.has(id) ? { setData: () => {} } : undefined,
    addSource: (id: string, spec: Record<string, unknown>) => {
      sources.set(id, spec);
    },
    removeSource: (id: string) => sources.delete(id),
    getLayer: (id: string) =>
      layers.has(id) ? { id, ...layers.get(id) } : undefined,
    addLayer: (spec: Record<string, unknown>) => {
      layers.set(spec.id as string, spec);
    },
    removeLayer: (id: string) => layers.delete(id),
    getFilter: (id: string) => layers.get(id)?.filter,
    setFilter: () => {},
    setPaintProperty: () => {},
    setLayoutProperty: () => {},
    setLayerZoomRange: () => {},
    moveLayer: () => {},
    getStyle: () => ({ layers: [], sources: Object.fromEntries(sources) }),
    once: () => {},
  };
  return { map, sources };
}

function geojsonLayer(attribution?: string): GeoLibreLayer {
  return {
    id: "lyr",
    name: "Layer",
    type: "geojson",
    source: { type: "geojson", ...(attribution ? { attribution } : {}) },
    visible: true,
    opacity: 1,
    style: { ...DEFAULT_LAYER_STYLE },
    metadata: {},
    geojson: {
      type: "FeatureCollection",
      features: [
        {
          type: "Feature",
          geometry: { type: "Point", coordinates: [0, 0] },
          properties: {},
        },
      ],
    },
  };
}

describe("syncGeoJsonLayer source attribution", () => {
  it("passes a layer's source attribution through to map.addSource", () => {
    const { map, sources } = makeMap();
    syncLayer(map as never, geojsonLayer("© Example City Data"));
    const geojsonSources = [...sources.values()].filter(
      (spec) => spec.type === "geojson",
    );
    assert.ok(geojsonSources.length > 0, "expected a geojson source");
    assert.ok(
      geojsonSources.some(
        (spec) => spec.attribution === "© Example City Data",
      ),
      "expected the source attribution to reach MapLibre",
    );
  });

  it("omits attribution when the layer declares none", () => {
    const { map, sources } = makeMap();
    syncLayer(map as never, geojsonLayer());
    const geojsonSource = [...sources.values()].find(
      (spec) => spec.type === "geojson",
    );
    assert.ok(geojsonSource, "expected a geojson source");
    assert.equal("attribution" in geojsonSource, false);
  });
});

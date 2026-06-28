# Native duckdb-rs vector engine on desktop

Date: 2026-06-28
Branch: `feat/native-duckdb-desktop-vector`
Closes: opengeos/GeoLibre#961, opengeos/GeoLibre#962

## Problem

On desktop the app is already Tauri/Rust, but all DuckDB-backed vector parsing
(Parquet/GeoParquet, and every `ST_Read` format) still runs in the webview via
`duckdb-wasm`. That inherits WASM's limits:

- ~4 GB memory ceiling and single-threaded constraints (cf. the GeoPackage
  `gpkg_ogr_contents` repair needed for the single-threaded WASM build).
- The `stoi: no conversion` GeoParquet bug (upstream duckdb/duckdb-wasm#2199) and
  the load-order warm-up hacks it forces (`parquetWarmUp`, `db.open({})`).
- Full in-memory buffering: the file's bytes are read into JS and handed to
  `registerFileBuffer` before any parsing.

## Goal

When running in the desktop (Tauri) shell, read DuckDB-backed vector files with a
native `duckdb` Rust crate (DuckDB v1.5.4, same line as the WASM pin) and return
the same WGS84 `FeatureCollection` the WASM path returns. The web build is
unchanged.

## Decisions (from brainstorming)

1. **Scope:** all DuckDB-backed vector formats on desktop, i.e. the formats that
   currently fall through to `loadDuckDbVector` in `loadTauriVectorFile`
   (parquet, geoparquet, shp+siblings, gpkg, fgb, CAD dxf/dwg, and any other
   `ST_Read`-readable format). The existing JS parsers for GeoJSON/KML/KMZ/GPX/CSV
   are unchanged.
2. **Spatial extension:** bundle per-platform signed `spatial.duckdb_extension`
   (osx arm64/x64, windows x64, linux x64/arm64) as Tauri resources and `LOAD`
   by path; fall back to network `INSTALL spatial` when the bundled file is
   missing for the current platform.
3. **Fallback:** on any native-path error, retry the same file through the
   existing `duckdb-wasm` loader; surface the WASM error only if that also fails.

## Boundary

- **Desktop only.** Web build untouched. The WASM warm-up hacks
  (`parquetWarmUp`, `db.open({})`, `sql-workspace.ts` remote warm-up) remain,
  they are still needed for the web path. #961 stays open as the upstream
  tracking issue; this work makes it not bite desktop.
- The native engine replaces exactly the `loadDuckDbVector` fallback branch in
  `apps/geolibre-desktop/src/lib/tauri-io.ts:loadTauriVectorFile`. Nothing else
  in the drop/dialog/ingestion flow changes.

## Architecture

### Rust (`src-tauri`)

- New module `src-tauri/src/duckdb_vector.rs` exposing one command,
  registered in the existing `invoke_handler!` in `lib.rs`:

  ```rust
  #[tauri::command]
  async fn load_native_vector(
      app: tauri::AppHandle,
      path: String,
      options: NativeVectorOptions, // { layer, override_source_crs, feature_warn_count, large_dataset_confirmed }
  ) -> Result<NativeVectorResult, String>
  ```

  `NativeVectorResult` is either the parsed FeatureCollection JSON string, or a
  structured `{ needsConfirmation: true, featureCount }` sentinel (see Data flow
  step 4).

- Dependency: `duckdb = { version = "1.10504.0", features = ["bundled"] }`
  (bundles DuckDB core v1.5.4). The crate re-exports Arrow as `duckdb::arrow`;
  no standalone arrow dep.
- Threading: query work runs inside `tauri::async_runtime::spawn_blocking`
  (`Connection` is `Send`, not `Sync`); a fresh in-memory connection per call.
  Config: `Config::default().allow_unsigned_extensions()?.enable_external_access(true)?`.
- Path validation reuses the existing `is_allowed_local_vector_path` allowlist
  in `lib.rs`.

### Spatial extension distribution

- `scripts/fetch-duckdb-spatial.mjs`: downloads and gunzips the v1.5.4
  `spatial.duckdb_extension` for the five targets into a resources directory.
- `tauri.conf.json` lists the per-platform binaries under `bundle.resources`.
- At runtime: resolve the resource path for the current platform via
  `app.path()`, `LOAD '<path>'`; if the resource is absent, `INSTALL spatial; LOAD spatial;`
  over the network.

### TypeScript

- New `apps/geolibre-desktop/src/lib/native-duckdb-vector.ts`: thin wrapper that
  `invoke("load_native_vector", …)`, parses the returned JSON string into a
  `FeatureCollection`, and handles the needs-confirmation round-trip.
- `loadTauriVectorFile` branch: when `isTauri()` and the file is a
  DuckDB-backed format, call the native wrapper; on any thrown error, fall back
  to the current `loadDuckDbVector(bytes)` WASM path, surfacing the WASM error
  only if that also fails.

## Data flow (desktop, per file)

1. Tauri drop/dialog yields an absolute **path** (already the case) →
   `loadTauriVectorFile`.
2. Native command opens an in-memory DuckDB, loads spatial, and builds the
   **same SQL** the WASM path builds (`sourceSql`: `read_parquet(path)` or
   `ST_Read(path, layer=…)`).
3. `DESCRIBE` → detect the geometry column with the same rules as
   `duckdb-geometry.ts:detectGeometryColumn` (prefer native GEOMETRY type, else
   well-known WKB blob names). Error `"DuckDB did not find a geometry column in
   this file."` if none.
4. `count(*)` over the source; if `> feature_warn_count` (default 500_000) and
   `large_dataset_confirmed` is false, return the needs-confirmation sentinel
   with the count. TS shows the existing confirm dialog, then re-invokes with
   `large_dataset_confirmed: true`.
5. Read source CRS via `ST_Read_Meta` (null for parquet, same as today).
6. Build the FeatureCollection **in SQL** and return it as one JSON string:

   ```sql
   SELECT json_object(
     'type', 'FeatureCollection',
     'features', coalesce(json_group_array(json_object(
       'type', 'Feature',
       'geometry', ST_AsGeoJSON(<transformed geom expr>)::JSON,
       'properties', to_json(data EXCLUDE (<geom col>, <detected blob cols>, OGC_FID))
     )), json_array())
   ) AS fc
   FROM (<sourceSql>) AS data
   ```

   `<transformed geom expr>` mirrors `geometryGeoJsonSql` +
   `geometryExpr`: `ST_GeomFromWKB(col)` when the column is a WKB blob, wrapped
   in `ST_Transform(…, srcCrs, 'EPSG:4326', true)` when a source CRS is known.
7. TS `JSON.parse` → `FeatureCollection` in WGS84 → unchanged `addGeoJsonLayer`.

Building the collection in SQL (same DuckDB engine) maximises parity. The
`EXCLUDE` of blob and `OGC_FID` columns mirrors `toFeatureCollection` skipping
`Uint8Array` properties and `stripAutoFidColumn`. If a property-normalization
edge fails parity against the golden fixtures, fall back to Rust-side row
building for that case.

## Error handling

- No geometry column, unreadable file, or extension load failure →
  command returns `Err(String)`.
- TS catches any native error → WASM retry → only if that also fails, show the
  existing error toast.
- Best-effort: keep behavior identical to today for anything native cannot
  handle.

## Testing

- **Rust** (`cargo test`): integration test reading small committed fixtures
  (a GeoParquet, a shapefile, a non-WGS84 GeoPackage) and asserting the emitted
  FeatureCollection JSON equals a golden, including reprojection to WGS84 and
  `OGC_FID` removal.
- **Parity:** reuse the `tests/vector-golden.test.ts` fixtures so native output
  is checked against the same goldens as the WASM/sidecar paths.
- **Frontend** unit test: `loadTauriVectorFile` selects native under `isTauri()`
  and falls back to WASM when the native `invoke` rejects (mocked `invoke`).
- **Manual:** `npm run tauri:dev`, drag-drop a GeoParquet, watch logs; confirm
  the `stoi` path no longer triggers on desktop.

## Out of scope (YAGNI)

- No Arrow-IPC transfer or streaming/paging yet; a single JSON string is enough
  for parity now. Streaming large layers is a clean follow-up.
- No web-build changes.
- No removal of the WASM warm-up hacks (still required for web).

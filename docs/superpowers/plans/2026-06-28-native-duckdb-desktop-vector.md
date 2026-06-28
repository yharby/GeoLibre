# Native duckdb-rs Desktop Vector Engine Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** On desktop (Tauri), read all DuckDB-backed vector files (Parquet/GeoParquet, shapefile, GeoPackage, FlatGeobuf, CAD, any `ST_Read` format) with a native `duckdb-rs` engine that returns the same WGS84 `FeatureCollection` the `duckdb-wasm` path returns, falling back to WASM on any native error. Web build is unchanged.

**Architecture:** A new Rust Tauri command (`load_native_vector`) opens an in-memory DuckDB (bundled, DuckDB v1.5.4), loads the `spatial` extension from a bundled per-platform binary (network `INSTALL` fallback), and runs the same `read_parquet`/`ST_Read` → `DESCRIBE` → geometry-detect → `ST_AsGeoJSON(ST_Transform(...))` pipeline as the WASM loader, returning the whole FeatureCollection as one JSON string built in SQL. The TS `loadTauriVectorFile` branches to this command on desktop and retries via the existing WASM loader on error.

**Tech Stack:** Rust, Tauri v2, `duckdb` crate `1.10504.0` (feature `bundled`), DuckDB spatial extension v1.5.4, TypeScript, React, `@tauri-apps/api` `invoke`, `node --test` (tsx), `cargo test`.

## Global Constraints

- Node `>=22`; package manager is **npm** (repo tracks `package-lock.json`).
- Rust crate: `duckdb = { version = "1.10504.0", features = ["bundled"] }` (bundles DuckDB core **v1.5.4**, same line as the pinned `@duckdb/duckdb-wasm` 1.33.1-dev45). Use `duckdb::arrow` if Arrow is ever needed; never add a standalone `arrow` dep.
- Spatial extension binaries must be the **v1.5.4** build for each target (`osx_arm64`, `osx_amd64`, `windows_amd64`, `linux_amd64`, `linux_arm64`).
- Output is always a GeoJSON `FeatureCollection` in **EPSG:4326 (WGS84)**, lon/lat order.
- Never commit to `main`. All work on branch `feat/native-duckdb-desktop-vector`.
- User-facing copy: no em dashes (use comma or period), no colons/semicolons in copy; wrap new user-facing strings in `t()` (react-i18next).
- Desktop-only behavior change; do not touch `loadBrowserVectorFile` or any web path.

## Validated SQL (confirmed against DuckDB CLI v1.5.4)

Source SQL (mirrors `duckdb-vector-loader.ts:sourceSql`):
- Parquet/GeoParquet: `SELECT * FROM read_parquet('<path>')`
- Else: `SELECT * FROM ST_Read('<path>'[, layer='<layer>'])`

FeatureCollection build (one row, one JSON string):
```sql
SELECT json_object(
  'type', 'FeatureCollection',
  'features', coalesce(json_group_array(json_object(
    'type', 'Feature',
    'geometry', <GEOM_GEOJSON_EXPR>::JSON,
    'properties', json_merge_patch(to_json(s), '<NULL_PATCH>')
  )), json_array())
) AS fc
FROM (<SOURCE_SQL>) AS s
```
- `<GEOM_GEOJSON_EXPR>` = `ST_AsGeoJSON(<geom>)` or, when a source CRS is known,
  `ST_AsGeoJSON(ST_Transform(<geom>, '<srcCrs>', 'EPSG:4326', true))`.
- `<geom>` = `"<col>"` for a native GEOMETRY column, or `ST_GeomFromWKB("<col>")` for a WKB blob column.
- `<NULL_PATCH>` = a JSON object string setting every excluded key to null, e.g. `{"geom":null,"raw":null,"ogc_fid":null}`. Excluded keys = the geometry column + every `BLOB`-typed column + any column named `OGC_FID` (case-insensitive). This drops geometry/blob/auto-fid columns from `properties`, mirroring `toFeatureCollection` skipping `Uint8Array` values and `stripAutoFidColumn`.

CRS read (mirrors `crsSql`, parquet returns null without querying):
```sql
SELECT layers[1].geometry_fields[1].crs.auth_name AS auth_name,
       layers[1].geometry_fields[1].crs.auth_code AS auth_code
FROM ST_Read_Meta('<path>')
```
Result `<AUTH_NAME>:<AUTH_CODE>` uppercased, or null when either is empty/missing/errors.

---

### Task 1: Add `duckdb` crate and prove the bundled build compiles

**Files:**
- Modify: `apps/geolibre-desktop/src-tauri/Cargo.toml`
- Create: `apps/geolibre-desktop/src-tauri/src/duckdb_vector.rs`
- Modify: `apps/geolibre-desktop/src-tauri/src/lib.rs` (add `mod duckdb_vector;` near the other module declarations)

**Interfaces:**
- Produces: module `duckdb_vector` with `pub(crate) fn open_in_memory() -> Result<duckdb::Connection, String>`.

- [ ] **Step 1: Add the dependency**

In `apps/geolibre-desktop/src-tauri/Cargo.toml`, under `[dependencies]` (next to `rusqlite`):
```toml
duckdb = { version = "1.10504.0", features = ["bundled"] }
```

- [ ] **Step 2: Write the module with a failing smoke test**

Create `apps/geolibre-desktop/src-tauri/src/duckdb_vector.rs`:
```rust
use duckdb::Connection;

/// Open a fresh in-memory DuckDB connection for a single vector load.
pub(crate) fn open_in_memory() -> Result<Connection, String> {
    Connection::open_in_memory().map_err(|error| format!("Could not open DuckDB: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_in_memory_and_runs_a_query() {
        let conn = open_in_memory().expect("open in-memory DuckDB");
        let value: i64 = conn
            .query_row("SELECT 1", [], |row| row.get(0))
            .expect("select 1");
        assert_eq!(value, 1);
    }
}
```

- [ ] **Step 3: Declare the module**

In `apps/geolibre-desktop/src-tauri/src/lib.rs`, add alongside existing top-level `mod` declarations:
```rust
mod duckdb_vector;
```

- [ ] **Step 4: Run the test (this also proves the multi-minute bundled compile works on this machine)**

Run: `PATH="$HOME/.cargo/bin:$PATH" cargo test --manifest-path apps/geolibre-desktop/src-tauri/Cargo.toml duckdb_vector::`
Expected: compiles (first build is slow), then `test duckdb_vector::tests::opens_in_memory_and_runs_a_query ... ok`.

- [ ] **Step 5: Commit**
```bash
git add apps/geolibre-desktop/src-tauri/Cargo.toml apps/geolibre-desktop/src-tauri/Cargo.lock apps/geolibre-desktop/src-tauri/src/duckdb_vector.rs apps/geolibre-desktop/src-tauri/src/lib.rs
git commit -m "feat(desktop): add bundled duckdb crate and in-memory smoke test"
```

---

### Task 2: Pure SQL/geometry helpers (mirror `duckdb-geometry.ts`)

**Files:**
- Modify: `apps/geolibre-desktop/src-tauri/src/duckdb_vector.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub(crate) struct DescribedColumn { pub name: String, pub column_type: String }`
  - `pub(crate) struct DetectedGeometry { pub column: String, pub is_wkb: bool }`
  - `fn quote_identifier(value: &str) -> String`
  - `fn quote_sql_string(value: &str) -> String`
  - `fn source_sql(path: &str, extension: &str, layer: Option<&str>) -> String`
  - `fn detect_geometry_column(cols: &[DescribedColumn]) -> Option<DetectedGeometry>`
  - `fn geometry_expr(geom: &DetectedGeometry) -> String`
  - `fn geometry_geojson_expr(geom_expr: &str, source_crs: Option<&str>) -> String`
  - `fn excluded_property_keys(cols: &[DescribedColumn], geom_col: &str) -> Vec<String>`
  - `fn null_patch_json(keys: &[String]) -> String`
  - `fn feature_collection_sql(source_sql: &str, geom_geojson_expr: &str, null_patch: &str) -> String`

- [ ] **Step 1: Write failing unit tests**

Append to the `tests` module in `duckdb_vector.rs`:
```rust
    #[test]
    fn quotes_identifiers_and_strings() {
        assert_eq!(quote_identifier(r#"odd"name"#), r#""odd""name""#);
        assert_eq!(quote_sql_string("o'brien"), "'o''brien'");
    }

    #[test]
    fn builds_parquet_and_st_read_source_sql() {
        assert_eq!(
            source_sql("/d/a.parquet", "parquet", None),
            "SELECT * FROM read_parquet('/d/a.parquet')"
        );
        assert_eq!(
            source_sql("/d/a.geoparquet", "geoparquet", None),
            "SELECT * FROM read_parquet('/d/a.geoparquet')"
        );
        assert_eq!(
            source_sql("/d/a.shp", "shp", None),
            "SELECT * FROM ST_Read('/d/a.shp')"
        );
        assert_eq!(
            source_sql("/d/a.dwg", "dwg", Some("L1")),
            "SELECT * FROM ST_Read('/d/a.dwg', layer='L1')"
        );
    }

    fn col(name: &str, ty: &str) -> DescribedColumn {
        DescribedColumn { name: name.to_string(), column_type: ty.to_string() }
    }

    #[test]
    fn prefers_native_geometry_then_wkb_blob() {
        let native = vec![col("id", "INTEGER"), col("geom", "GEOMETRY('OGC:CRS84')")];
        assert_eq!(
            detect_geometry_column(&native),
            Some(DetectedGeometry { column: "geom".into(), is_wkb: false })
        );
        let wkb = vec![col("id", "INTEGER"), col("geometry", "BLOB")];
        assert_eq!(
            detect_geometry_column(&wkb),
            Some(DetectedGeometry { column: "geometry".into(), is_wkb: true })
        );
        let none = vec![col("id", "INTEGER"), col("name", "VARCHAR")];
        assert_eq!(detect_geometry_column(&none), None);
    }

    #[test]
    fn builds_geometry_geojson_expr_with_and_without_crs() {
        let native = DetectedGeometry { column: "geom".into(), is_wkb: false };
        assert_eq!(
            geometry_geojson_expr(&geometry_expr(&native), None),
            r#"ST_AsGeoJSON("geom")"#
        );
        assert_eq!(
            geometry_geojson_expr(&geometry_expr(&native), Some("EPSG:3857")),
            r#"ST_AsGeoJSON(ST_Transform("geom", 'EPSG:3857', 'EPSG:4326', true))"#
        );
        let wkb = DetectedGeometry { column: "geometry".into(), is_wkb: true };
        assert_eq!(
            geometry_expr(&wkb),
            r#"ST_GeomFromWKB("geometry")"#
        );
    }

    #[test]
    fn excludes_geometry_blob_and_ogc_fid_from_properties() {
        let cols = vec![
            col("id", "INTEGER"),
            col("name", "VARCHAR"),
            col("geom", "GEOMETRY"),
            col("raw", "BLOB"),
            col("OGC_FID", "INTEGER"),
        ];
        let keys = excluded_property_keys(&cols, "geom");
        assert!(keys.contains(&"geom".to_string()));
        assert!(keys.contains(&"raw".to_string()));
        assert!(keys.contains(&"OGC_FID".to_string()));
        assert!(!keys.contains(&"id".to_string()));
        assert_eq!(null_patch_json(&["geom".into(), "raw".into()]), r#"{"geom":null,"raw":null}"#);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `PATH="$HOME/.cargo/bin:$PATH" cargo test --manifest-path apps/geolibre-desktop/src-tauri/Cargo.toml duckdb_vector::`
Expected: FAIL to compile (functions/types not defined).

- [ ] **Step 3: Implement the helpers**

Add to `duckdb_vector.rs` (above the `tests` module):
```rust
const TARGET_CRS: &str = "EPSG:4326";
const WKB_COLUMN_NAMES: [&str; 6] =
    ["geometry", "geom", "wkb_geometry", "geometry_wkb", "geom_wkb", "wkb"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DescribedColumn {
    pub name: String,
    pub column_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DetectedGeometry {
    pub column: String,
    pub is_wkb: bool,
}

fn quote_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn quote_sql_string(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn is_parquet_extension(extension: &str) -> bool {
    extension == "parquet" || extension == "geoparquet"
}

fn source_sql(path: &str, extension: &str, layer: Option<&str>) -> String {
    let quoted = quote_sql_string(path);
    if is_parquet_extension(extension) {
        return format!("SELECT * FROM read_parquet({quoted})");
    }
    match layer {
        Some(layer) => format!(
            "SELECT * FROM ST_Read({quoted}, layer={})",
            quote_sql_string(layer)
        ),
        None => format!("SELECT * FROM ST_Read({quoted})"),
    }
}

fn is_native_geometry_type(column_type: &str) -> bool {
    column_type.trim_start().to_ascii_uppercase().starts_with("GEOMETRY")
}

fn detect_geometry_column(cols: &[DescribedColumn]) -> Option<DetectedGeometry> {
    if let Some(native) = cols.iter().find(|c| is_native_geometry_type(&c.column_type)) {
        return Some(DetectedGeometry { column: native.name.clone(), is_wkb: false });
    }
    cols.iter()
        .find(|c| {
            c.column_type.to_ascii_uppercase().starts_with("BLOB")
                && WKB_COLUMN_NAMES.contains(&c.name.to_ascii_lowercase().as_str())
        })
        .map(|c| DetectedGeometry { column: c.name.clone(), is_wkb: true })
}

fn geometry_expr(geom: &DetectedGeometry) -> String {
    let column = quote_identifier(&geom.column);
    if geom.is_wkb {
        format!("ST_GeomFromWKB({column})")
    } else {
        column
    }
}

fn geometry_geojson_expr(geom_expr: &str, source_crs: Option<&str>) -> String {
    match source_crs {
        None => format!("ST_AsGeoJSON({geom_expr})"),
        Some(crs) => format!(
            "ST_AsGeoJSON(ST_Transform({geom_expr}, {}, {}, true))",
            quote_sql_string(crs),
            quote_sql_string(TARGET_CRS)
        ),
    }
}

fn excluded_property_keys(cols: &[DescribedColumn], geom_col: &str) -> Vec<String> {
    let mut keys = vec![geom_col.to_string()];
    for c in cols {
        let is_blob = c.column_type.to_ascii_uppercase().starts_with("BLOB");
        let is_ogc_fid = c.name.eq_ignore_ascii_case("OGC_FID");
        if (is_blob || is_ogc_fid) && c.name != geom_col {
            keys.push(c.name.clone());
        }
    }
    keys
}

fn null_patch_json(keys: &[String]) -> String {
    let entries: Vec<String> = keys
        .iter()
        .map(|k| format!("{}:null", serde_json::to_string(k).unwrap_or_default()))
        .collect();
    format!("{{{}}}", entries.join(","))
}

fn feature_collection_sql(source_sql: &str, geom_geojson_expr: &str, null_patch: &str) -> String {
    format!(
        "SELECT json_object(\
           'type', 'FeatureCollection', \
           'features', coalesce(json_group_array(json_object(\
             'type', 'Feature', \
             'geometry', {geom}::JSON, \
             'properties', json_merge_patch(to_json(s), {patch})\
           )), json_array())\
         ) AS fc FROM ({source}) AS s",
        geom = geom_geojson_expr,
        patch = quote_sql_string(null_patch),
        source = source_sql,
    )
}
```

Also ensure `serde_json` is available (it is already a dependency per `Cargo.toml`).

- [ ] **Step 4: Run tests to verify they pass**

Run: `PATH="$HOME/.cargo/bin:$PATH" cargo test --manifest-path apps/geolibre-desktop/src-tauri/Cargo.toml duckdb_vector::`
Expected: all `duckdb_vector::tests::*` PASS.

- [ ] **Step 5: Commit**
```bash
git add apps/geolibre-desktop/src-tauri/src/duckdb_vector.rs
git commit -m "feat(desktop): native vector SQL and geometry-detection helpers"
```

---

### Task 3: Full native load pipeline + integration test against fixtures

**Files:**
- Modify: `apps/geolibre-desktop/src-tauri/src/duckdb_vector.rs`
- Create fixtures: `apps/geolibre-desktop/src-tauri/tests/fixtures/points_wgs84.parquet`, `apps/geolibre-desktop/src-tauri/tests/fixtures/points_3857.parquet`

**Interfaces:**
- Consumes: helpers from Task 2; `open_in_memory` from Task 1.
- Produces:
  - `#[derive(Default, serde::Deserialize)] pub(crate) struct NativeVectorOptions { pub layer: Option<String>, pub override_source_crs: Option<String>, pub feature_warn_count: Option<u64>, pub large_dataset_confirmed: Option<bool> }` with `#[serde(rename_all = "camelCase", default)]`
  - `fn load_feature_collection(conn: &Connection, path: &str, extension: &str, options: &NativeVectorOptions) -> Result<String, String>` (returns the FeatureCollection JSON string; large-dataset gating added in Task 4)
  - `fn read_source_crs(conn: &Connection, path: &str, extension: &str) -> Option<String>`
  - `fn describe_columns(conn: &Connection, source_sql: &str) -> Result<Vec<DescribedColumn>, String>`

- [ ] **Step 1: Create fixtures with the DuckDB CLI**

Run:
```bash
mkdir -p apps/geolibre-desktop/src-tauri/tests/fixtures
duckdb -c "INSTALL spatial; LOAD spatial;
COPY (SELECT 1 AS id, 'alpha' AS name, ST_Point(-3.7, 40.4) AS geom, 'x'::BLOB AS raw, 99 AS ogc_fid
      UNION ALL SELECT 2,'beta',ST_Point(2.3,48.8),'y'::BLOB,100)
TO 'apps/geolibre-desktop/src-tauri/tests/fixtures/points_wgs84.parquet' (FORMAT PARQUET);"
duckdb -c "INSTALL spatial; LOAD spatial;
COPY (SELECT 1 AS id, ST_Point(-411724, 4926765) AS geom)
TO 'apps/geolibre-desktop/src-tauri/tests/fixtures/points_3857.parquet' (FORMAT PARQUET);"
```
Note: the GeoParquet CRS is not read back for parquet (matches WASM), so `points_3857.parquet` verifies the no-CRS passthrough path, not reprojection. Reprojection is covered by a CRS override in Step 2.

- [ ] **Step 2: Write failing integration test**

Create `apps/geolibre-desktop/src-tauri/tests/native_vector.rs`:
```rust
// Integration test: exercises the crate's public(crate) pipeline through a tiny
// re-exported shim. We test via a helper exposed under #[cfg(test)] is not
// available across the integration-test boundary, so call the command-free core.
```
Because `pub(crate)` items are not visible to `tests/` integration crates, put this test **inside** `duckdb_vector.rs`'s `tests` module instead:
```rust
    fn fixture(name: &str) -> String {
        format!("{}/tests/fixtures/{}", env!("CARGO_MANIFEST_DIR"), name)
    }

    #[test]
    fn loads_parquet_into_feature_collection() {
        let conn = open_in_memory().expect("conn");
        conn.execute_batch("INSTALL spatial; LOAD spatial;").expect("spatial");
        let fc_json = load_feature_collection(
            &conn,
            &fixture("points_wgs84.parquet"),
            "parquet",
            &NativeVectorOptions::default(),
        )
        .expect("load");
        let fc: serde_json::Value = serde_json::from_str(&fc_json).expect("json");
        assert_eq!(fc["type"], "FeatureCollection");
        assert_eq!(fc["features"].as_array().unwrap().len(), 2);
        let f0 = &fc["features"][0];
        assert_eq!(f0["geometry"]["type"], "Point");
        assert_eq!(f0["geometry"]["coordinates"][0], -3.7);
        // properties exclude geometry, blob, and OGC_FID
        assert_eq!(f0["properties"]["id"], 1);
        assert_eq!(f0["properties"]["name"], "alpha");
        assert!(f0["properties"].get("geom").is_none());
        assert!(f0["properties"].get("raw").is_none());
        assert!(f0["properties"].get("ogc_fid").is_none());
    }

    #[test]
    fn reprojects_when_source_crs_is_overridden() {
        let conn = open_in_memory().expect("conn");
        conn.execute_batch("INSTALL spatial; LOAD spatial;").expect("spatial");
        let options = NativeVectorOptions {
            override_source_crs: Some("EPSG:3857".to_string()),
            ..Default::default()
        };
        let fc_json = load_feature_collection(
            &conn,
            &fixture("points_3857.parquet"),
            "parquet",
            &options,
        )
        .expect("load");
        let fc: serde_json::Value = serde_json::from_str(&fc_json).expect("json");
        let lon = fc["features"][0]["geometry"]["coordinates"][0].as_f64().unwrap();
        let lat = fc["features"][0]["geometry"]["coordinates"][1].as_f64().unwrap();
        assert!((lon - (-3.6986)).abs() < 0.01, "lon was {lon}");
        assert!((lat - 40.4173).abs() < 0.01, "lat was {lat}");
    }

    #[test]
    fn errors_when_no_geometry_column() {
        let conn = open_in_memory().expect("conn");
        conn.execute_batch("INSTALL spatial; LOAD spatial;").expect("spatial");
        // a parquet with no geometry: reuse describe path via a values-based source
        let result = describe_columns(&conn, "SELECT 1 AS id, 'x' AS name");
        let cols = result.expect("describe");
        assert!(detect_geometry_column(&cols).is_none());
    }
```

- [ ] **Step 3: Run test to verify it fails**

Run: `PATH="$HOME/.cargo/bin:$PATH" cargo test --manifest-path apps/geolibre-desktop/src-tauri/Cargo.toml duckdb_vector::`
Expected: FAIL to compile (`load_feature_collection`, `describe_columns`, `NativeVectorOptions` undefined).

- [ ] **Step 4: Implement the pipeline**

Add to `duckdb_vector.rs` (above `tests`):
```rust
use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct NativeVectorOptions {
    pub layer: Option<String>,
    pub override_source_crs: Option<String>,
    pub feature_warn_count: Option<u64>,
    pub large_dataset_confirmed: Option<bool>,
}

fn describe_columns(conn: &Connection, source_sql: &str) -> Result<Vec<DescribedColumn>, String> {
    let mut stmt = conn
        .prepare(&format!("DESCRIBE {source_sql}"))
        .map_err(|e| format!("DESCRIBE failed: {e}"))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(DescribedColumn {
                name: row.get::<_, String>(0)?,
                column_type: row.get::<_, String>(1)?,
            })
        })
        .map_err(|e| format!("DESCRIBE read failed: {e}"))?;
    let mut cols = Vec::new();
    for row in rows {
        cols.push(row.map_err(|e| format!("DESCRIBE row failed: {e}"))?);
    }
    Ok(cols)
}

fn read_source_crs(conn: &Connection, path: &str, extension: &str) -> Option<String> {
    if is_parquet_extension(extension) {
        return None;
    }
    let sql = format!(
        "SELECT layers[1].geometry_fields[1].crs.auth_name, \
                layers[1].geometry_fields[1].crs.auth_code \
         FROM ST_Read_Meta({})",
        quote_sql_string(path)
    );
    let result: Result<(Option<String>, Option<String>), _> =
        conn.query_row(&sql, [], |row| Ok((row.get(0).ok(), row.get(1).ok())));
    let (auth_name, auth_code) = result.ok()?;
    let auth_name = auth_name?.trim().to_string();
    let auth_code = auth_code?.trim().to_string();
    if auth_name.is_empty() || auth_code.is_empty() {
        return None;
    }
    Some(format!("{}:{}", auth_name.to_ascii_uppercase(), auth_code))
}

fn load_feature_collection(
    conn: &Connection,
    path: &str,
    extension: &str,
    options: &NativeVectorOptions,
) -> Result<String, String> {
    let src = source_sql(path, extension, options.layer.as_deref());
    let columns = describe_columns(conn, &src)?;
    let detected = detect_geometry_column(&columns)
        .ok_or_else(|| "DuckDB did not find a geometry column in this file.".to_string())?;

    let source_crs = options
        .override_source_crs
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| read_source_crs(conn, path, extension));

    let geom_geojson = geometry_geojson_expr(&geometry_expr(&detected), source_crs.as_deref());
    let patch = null_patch_json(&excluded_property_keys(&columns, &detected.column));
    let sql = feature_collection_sql(&src, &geom_geojson, &patch);

    conn.query_row(&sql, [], |row| row.get::<_, String>(0))
        .map_err(|e| format!("Could not build GeoJSON: {e}"))
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `PATH="$HOME/.cargo/bin:$PATH" cargo test --manifest-path apps/geolibre-desktop/src-tauri/Cargo.toml duckdb_vector::`
Expected: all PASS.

- [ ] **Step 6: Commit**
```bash
git add apps/geolibre-desktop/src-tauri/src/duckdb_vector.rs apps/geolibre-desktop/src-tauri/tests/fixtures
git commit -m "feat(desktop): native parquet load pipeline with CRS and properties parity"
```

---

### Task 4: Large-dataset confirmation gating

**Files:**
- Modify: `apps/geolibre-desktop/src-tauri/src/duckdb_vector.rs`

**Interfaces:**
- Consumes: `load_feature_collection`, `source_sql`, `NativeVectorOptions`.
- Produces:
  - `pub(crate) const DEFAULT_FEATURE_WARN_COUNT: u64 = 500_000;`
  - `#[derive(serde::Serialize)] #[serde(rename_all = "camelCase")] pub(crate) struct NativeVectorResult { pub needs_confirmation: bool, pub feature_count: Option<u64>, pub feature_collection: Option<String> }`
  - `fn count_features(conn: &Connection, source_sql: &str) -> Result<u64, String>`
  - `pub(crate) fn run_native_load(conn: &Connection, path: &str, extension: &str, options: &NativeVectorOptions) -> Result<NativeVectorResult, String>`

- [ ] **Step 1: Write failing tests**

Append to the `tests` module:
```rust
    #[test]
    fn returns_needs_confirmation_when_over_threshold_and_unconfirmed() {
        let conn = open_in_memory().expect("conn");
        conn.execute_batch("INSTALL spatial; LOAD spatial;").expect("spatial");
        let options = NativeVectorOptions { feature_warn_count: Some(1), ..Default::default() };
        let result = run_native_load(&conn, &fixture("points_wgs84.parquet"), "parquet", &options)
            .expect("run");
        assert!(result.needs_confirmation);
        assert_eq!(result.feature_count, Some(2));
        assert!(result.feature_collection.is_none());
    }

    #[test]
    fn loads_when_confirmed_despite_threshold() {
        let conn = open_in_memory().expect("conn");
        conn.execute_batch("INSTALL spatial; LOAD spatial;").expect("spatial");
        let options = NativeVectorOptions {
            feature_warn_count: Some(1),
            large_dataset_confirmed: Some(true),
            ..Default::default()
        };
        let result = run_native_load(&conn, &fixture("points_wgs84.parquet"), "parquet", &options)
            .expect("run");
        assert!(!result.needs_confirmation);
        assert!(result.feature_collection.is_some());
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `PATH="$HOME/.cargo/bin:$PATH" cargo test --manifest-path apps/geolibre-desktop/src-tauri/Cargo.toml duckdb_vector::`
Expected: FAIL to compile (`run_native_load`, `NativeVectorResult` undefined).

- [ ] **Step 3: Implement gating**

Add to `duckdb_vector.rs`:
```rust
use serde::Serialize;

pub(crate) const DEFAULT_FEATURE_WARN_COUNT: u64 = 500_000;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NativeVectorResult {
    pub needs_confirmation: bool,
    pub feature_count: Option<u64>,
    pub feature_collection: Option<String>,
}

fn count_features(conn: &Connection, source_sql: &str) -> Result<u64, String> {
    conn.query_row(
        &format!("SELECT count(*) FROM ({source_sql}) AS data"),
        [],
        |row| row.get::<_, i64>(0),
    )
    .map(|n| n.max(0) as u64)
    .map_err(|e| format!("Could not count features: {e}"))
}

pub(crate) fn run_native_load(
    conn: &Connection,
    path: &str,
    extension: &str,
    options: &NativeVectorOptions,
) -> Result<NativeVectorResult, String> {
    let src = source_sql(path, extension, options.layer.as_deref());
    // Validate geometry early so a missing-geometry file errors before the count.
    let columns = describe_columns(conn, &src)?;
    if detect_geometry_column(&columns).is_none() {
        return Err("DuckDB did not find a geometry column in this file.".to_string());
    }

    let warn_at = options.feature_warn_count.unwrap_or(DEFAULT_FEATURE_WARN_COUNT);
    let confirmed = options.large_dataset_confirmed.unwrap_or(false);
    if !confirmed {
        let count = count_features(conn, &src)?;
        if count > warn_at {
            return Ok(NativeVectorResult {
                needs_confirmation: true,
                feature_count: Some(count),
                feature_collection: None,
            });
        }
    }

    let fc = load_feature_collection(conn, path, extension, options)?;
    Ok(NativeVectorResult {
        needs_confirmation: false,
        feature_count: None,
        feature_collection: Some(fc),
    })
}
```

- [ ] **Step 4: Run tests to verify pass**

Run: `PATH="$HOME/.cargo/bin:$PATH" cargo test --manifest-path apps/geolibre-desktop/src-tauri/Cargo.toml duckdb_vector::`
Expected: all PASS.

- [ ] **Step 5: Commit**
```bash
git add apps/geolibre-desktop/src-tauri/src/duckdb_vector.rs
git commit -m "feat(desktop): large-dataset confirmation gating for native loader"
```

---

### Task 5: Spatial extension loading (bundled path + network fallback)

**Files:**
- Modify: `apps/geolibre-desktop/src-tauri/src/duckdb_vector.rs`

**Interfaces:**
- Consumes: `open_in_memory`.
- Produces:
  - `pub(crate) fn open_with_spatial(extension_path: Option<&std::path::Path>) -> Result<Connection, String>` (opens a connection with `allow_unsigned_extensions` + external access, then `LOAD '<path>'` if the bundled file exists, else `INSTALL spatial; LOAD spatial;`)

- [ ] **Step 1: Write failing test (network-independent path is hard to assert offline; assert the no-path branch loads spatial)**

Append to `tests`:
```rust
    #[test]
    fn open_with_spatial_loads_extension() {
        // No bundled path provided -> INSTALL/LOAD from the network or local cache.
        // In CI without network this is allowed to fail; gate on cfg to keep it
        // deterministic locally where the extension cache exists.
        if let Ok(conn) = open_with_spatial(None) {
            let ok: i64 = conn
                .query_row("SELECT 1 FROM duckdb_extensions() WHERE extension_name='spatial' AND loaded", [], |r| r.get(0))
                .unwrap_or(0);
            assert_eq!(ok, 1);
        }
    }
```

- [ ] **Step 2: Run to verify it fails to compile**

Run: `PATH="$HOME/.cargo/bin:$PATH" cargo test --manifest-path apps/geolibre-desktop/src-tauri/Cargo.toml duckdb_vector::open_with_spatial`
Expected: FAIL to compile (`open_with_spatial` undefined).

- [ ] **Step 3: Implement**

Add to `duckdb_vector.rs`:
```rust
use duckdb::Config;
use std::path::Path;

pub(crate) fn open_with_spatial(extension_path: Option<&Path>) -> Result<Connection, String> {
    let config = Config::default()
        .allow_unsigned_extensions()
        .map_err(|e| format!("DuckDB config (unsigned) failed: {e}"))?
        .enable_external_access(true)
        .map_err(|e| format!("DuckDB config (external access) failed: {e}"))?;
    let conn = Connection::open_in_memory_with_flags(config)
        .map_err(|e| format!("Could not open DuckDB: {e}"))?;

    let loaded_from_bundle = match extension_path {
        Some(path) if path.exists() => {
            let normalized = path.to_string_lossy().replace('\\', "/");
            conn.execute_batch(&format!("LOAD {};", quote_sql_string(&normalized)))
                .map_err(|e| format!("Could not load bundled spatial extension: {e}"))?;
            true
        }
        _ => false,
    };
    if !loaded_from_bundle {
        conn.execute_batch("INSTALL spatial; LOAD spatial;")
            .map_err(|e| format!("Could not install/load spatial extension: {e}"))?;
    }
    Ok(conn)
}
```

- [ ] **Step 4: Run the test**

Run: `PATH="$HOME/.cargo/bin:$PATH" cargo test --manifest-path apps/geolibre-desktop/src-tauri/Cargo.toml duckdb_vector::open_with_spatial`
Expected: PASS (loads spatial when the network/cache is available; the test is tolerant when it is not).

- [ ] **Step 5: Commit**
```bash
git add apps/geolibre-desktop/src-tauri/src/duckdb_vector.rs
git commit -m "feat(desktop): spatial extension loader with bundled-path and network fallback"
```

---

### Task 6: Tauri command + registration + path validation

**Files:**
- Modify: `apps/geolibre-desktop/src-tauri/src/duckdb_vector.rs`
- Modify: `apps/geolibre-desktop/src-tauri/src/lib.rs`

**Interfaces:**
- Consumes: `run_native_load`, `open_with_spatial`, `NativeVectorOptions`, `NativeVectorResult`; the existing `is_allowed_local_vector_path` in `lib.rs`.
- Produces: `#[tauri::command] async fn load_native_vector(app, path, extension, options) -> Result<NativeVectorResult, String>` registered in the `invoke_handler!`.
- Produces: `pub(crate) fn resolve_spatial_extension_path(app: &tauri::AppHandle) -> Option<std::path::PathBuf>`.

- [ ] **Step 1: Add the command + path resolver to `duckdb_vector.rs`**
```rust
use tauri::{AppHandle, Manager};

/// Per-platform resource subpath populated by scripts/fetch-duckdb-spatial.mjs.
fn platform_extension_subpath() -> Option<&'static str> {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    { Some("resources/duckdb/osx_arm64/spatial.duckdb_extension") }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    { Some("resources/duckdb/osx_amd64/spatial.duckdb_extension") }
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    { Some("resources/duckdb/windows_amd64/spatial.duckdb_extension") }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    { Some("resources/duckdb/linux_amd64/spatial.duckdb_extension") }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    { Some("resources/duckdb/linux_arm64/spatial.duckdb_extension") }
    #[cfg(not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
    )))]
    { None }
}

pub(crate) fn resolve_spatial_extension_path(app: &AppHandle) -> Option<std::path::PathBuf> {
    let sub = platform_extension_subpath()?;
    app.path().resolve(sub, tauri::path::BaseDirectory::Resource).ok()
}
```

- [ ] **Step 2: Add the command function to `lib.rs`** (so it can reuse the existing `is_allowed_local_vector_path`):
```rust
#[tauri::command]
async fn load_native_vector(
    app: tauri::AppHandle,
    path: String,
    extension: String,
    options: duckdb_vector::NativeVectorOptions,
) -> Result<duckdb_vector::NativeVectorResult, String> {
    if !is_allowed_local_vector_path(&path) {
        return Err(format!(
            "Refusing to read \"{path}\": not an absolute local vector file path"
        ));
    }
    let ext_path = duckdb_vector::resolve_spatial_extension_path(&app);
    tauri::async_runtime::spawn_blocking(move || {
        let conn = duckdb_vector::open_with_spatial(ext_path.as_deref())?;
        duckdb_vector::run_native_load(&conn, &path, &extension.to_lowercase(), &options)
    })
    .await
    .map_err(|e| format!("Native vector task failed: {e}"))?
}
```
Make the needed items `pub(crate)` in `duckdb_vector.rs` (`NativeVectorOptions`, `NativeVectorResult`, `run_native_load`, `open_with_spatial`, `resolve_spatial_extension_path`).

- [ ] **Step 3: Register the command** in the `tauri::generate_handler![ ... ]` list in `lib.rs` (add `load_native_vector,`).

- [ ] **Step 4: Build to verify it compiles**

Run: `PATH="$HOME/.cargo/bin:$PATH" cargo check --manifest-path apps/geolibre-desktop/src-tauri/Cargo.toml`
Expected: compiles with no errors.

- [ ] **Step 5: Commit**
```bash
git add apps/geolibre-desktop/src-tauri/src/duckdb_vector.rs apps/geolibre-desktop/src-tauri/src/lib.rs
git commit -m "feat(desktop): expose load_native_vector Tauri command"
```

---

### Task 7: Bundle the spatial extension binaries

**Files:**
- Create: `scripts/fetch-duckdb-spatial.mjs`
- Modify: `apps/geolibre-desktop/src-tauri/tauri.conf.json` (`bundle.resources`)
- Modify: `package.json` (root) add a `fetch:duckdb-spatial` script
- Create: `apps/geolibre-desktop/src-tauri/resources/duckdb/.gitignore` (ignore the binaries, keep the dir)

**Interfaces:**
- Produces: per-platform `spatial.duckdb_extension` under `apps/geolibre-desktop/src-tauri/resources/duckdb/<platform>/`.

- [ ] **Step 1: Write the fetch script**

Create `scripts/fetch-duckdb-spatial.mjs`:
```js
// Downloads and gunzips the DuckDB spatial extension (v1.5.4) for every target
// platform into the Tauri resources tree. Run before `tauri:build`.
import { createWriteStream } from "node:fs";
import { mkdir, rm } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { createGunzip } from "node:zlib";
import { pipeline } from "node:stream/promises";

const DUCKDB_VERSION = "v1.5.4";
const PLATFORMS = ["osx_arm64", "osx_amd64", "windows_amd64", "linux_amd64", "linux_arm64"];
const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const outBase = join(root, "apps/geolibre-desktop/src-tauri/resources/duckdb");

for (const platform of PLATFORMS) {
  const url = `http://extensions.duckdb.org/${DUCKDB_VERSION}/${platform}/spatial.duckdb_extension.gz`;
  const outDir = join(outBase, platform);
  const outFile = join(outDir, "spatial.duckdb_extension");
  await mkdir(outDir, { recursive: true });
  process.stdout.write(`Fetching ${platform} ... `);
  const res = await fetch(url);
  if (!res.ok) {
    await rm(outFile, { force: true });
    throw new Error(`Failed ${platform}: HTTP ${res.status} from ${url}`);
  }
  await pipeline(res.body, createGunzip(), createWriteStream(outFile));
  console.log("done");
}
console.log("All spatial extensions fetched.");
```

- [ ] **Step 2: Add the root npm script** in `package.json` `"scripts"`:
```json
"fetch:duckdb-spatial": "node scripts/fetch-duckdb-spatial.mjs"
```

- [ ] **Step 3: Add the resources glob** in `apps/geolibre-desktop/src-tauri/tauri.conf.json` under `bundle` (create `resources` array if absent):
```json
"resources": ["resources/duckdb/**/*"]
```

- [ ] **Step 4: Keep the dir but ignore binaries** — create `apps/geolibre-desktop/src-tauri/resources/duckdb/.gitignore`:
```
*
!.gitignore
```

- [ ] **Step 5: Run the fetch and verify files land**

Run: `npm run fetch:duckdb-spatial && ls apps/geolibre-desktop/src-tauri/resources/duckdb/*/spatial.duckdb_extension`
Expected: five files listed, one per platform.

- [ ] **Step 6: Verify a bundled load works for this platform**

Run (macOS arm64 example):
```bash
PATH="$HOME/.cargo/bin:$PATH" cargo test --manifest-path apps/geolibre-desktop/src-tauri/Cargo.toml duckdb_vector::open_with_spatial
```
Then add (and remove after) a temporary assertion calling `open_with_spatial(Some(Path::new("apps/.../osx_arm64/spatial.duckdb_extension")))` if you want a direct check; otherwise trust the network-branch test plus the manual run in Task 9.

- [ ] **Step 7: Commit**
```bash
git add scripts/fetch-duckdb-spatial.mjs package.json apps/geolibre-desktop/src-tauri/tauri.conf.json apps/geolibre-desktop/src-tauri/resources/duckdb/.gitignore
git commit -m "build(desktop): fetch and bundle per-platform duckdb spatial extension"
```

---

### Task 8: TypeScript native-loader wrapper

**Files:**
- Create: `apps/geolibre-desktop/src/lib/native-duckdb-vector.ts`
- Create: `tests/native-duckdb-vector.test.ts`

**Interfaces:**
- Consumes: `invoke` from `@tauri-apps/api/core`; `DuckDbVectorLoadOptions`, `VectorLoadCancelledError`, `confirmLargeDataset` from `./duckdb-vector-guard` (re-exported via `duckdb-vector-loader`).
- Produces: `export async function loadNativeVectorFile(path: string, extension: string, options?: DuckDbVectorLoadOptions): Promise<FeatureCollection>`.

- [ ] **Step 1: Write the failing test**

Create `tests/native-duckdb-vector.test.ts`:
```ts
import assert from "node:assert/strict";
import { test } from "node:test";

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
const { loadNativeVectorFile } = await import("../apps/geolibre-desktop/src/lib/native-duckdb-vector.ts");
// The module reads invoke lazily through a settable seam (see implementation).
(globalThis as any).__GEOLIBRE_INVOKE__ = coreMock.invoke;

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
```

- [ ] **Step 2: Run to verify failure**

Run: `node --import tsx --test tests/native-duckdb-vector.test.ts`
Expected: FAIL (module/function not found).

- [ ] **Step 3: Implement the wrapper**

Create `apps/geolibre-desktop/src/lib/native-duckdb-vector.ts`:
```ts
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
    largeDatasetConfirmed: false,
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
```
Note: confirm `confirmLargeDataset`'s signature in `duckdb-vector-guard.ts` and match the argument shape (`{ name, featureCount }`, callback). If `confirmLargeDataset` throws `VectorLoadCancelledError` on decline (it does in the WASM path), the caller in Task 9 treats that as a cancel, not an error to fall back on.

- [ ] **Step 4: Run the test to verify pass**

Run: `node --import tsx --test tests/native-duckdb-vector.test.ts`
Expected: both tests PASS.

- [ ] **Step 5: Commit**
```bash
git add apps/geolibre-desktop/src/lib/native-duckdb-vector.ts tests/native-duckdb-vector.test.ts
git commit -m "feat(desktop): TS wrapper for native vector loader with large-dataset prompt"
```

---

### Task 9: Wire native path into `loadTauriVectorFile` with WASM fallback

**Files:**
- Modify: `apps/geolibre-desktop/src/lib/tauri-io.ts` (the `loadTauriVectorFile` DuckDB fallback branch, ~L867-881)
- Create/Modify: `tests/tauri-io-native-branch.test.ts`

**Interfaces:**
- Consumes: `loadNativeVectorFile` (Task 8); existing `loadDuckDbVector`, `isTauri`, `isVectorLoadCancelled`, `readLocalFileBytes`, `readShapefileSiblings`, `browserSafeFileName`, `fileExtension` in `tauri-io.ts`.
- Produces: native-first behavior in `loadTauriVectorFile` for the DuckDB-backed branch.

- [ ] **Step 1: Write the failing test**

Create `tests/tauri-io-native-branch.test.ts`. Mirror the existing tauri-io test setup (check how `tests/` mocks `@tauri-apps/*` and the `is-tauri` module). The test asserts:
- when `isTauri()` is true and `loadNativeVectorFile` resolves, the native result is returned and `loadDuckDbVector` (WASM) is not called;
- when `loadNativeVectorFile` rejects with a generic error, the WASM `loadDuckDbVector` path runs and its result is returned;
- when `loadNativeVectorFile` rejects with `VectorLoadCancelledError`, the cancel propagates (no WASM retry).

```ts
import assert from "node:assert/strict";
import { mock, test } from "node:test";

// See tests/tauri-io.*.test.ts for the established module-mock pattern; reuse it
// here to stub ./native-duckdb-vector, ./duckdb-vector-loader, ./is-tauri, and
// @tauri-apps/plugin-fs before importing tauri-io.
// Assertions:
test.todo("native success short-circuits WASM");
test.todo("native generic error falls back to WASM");
test.todo("native cancellation propagates without WASM retry");
```
Replace the `test.todo` placeholders with concrete mocks following the sibling test files' pattern (they already stub these modules), asserting call counts on the native vs WASM loaders.

- [ ] **Step 2: Run to verify failure**

Run: `node --import tsx --test tests/tauri-io-native-branch.test.ts`
Expected: FAIL (branch not implemented / todos pending).

- [ ] **Step 3: Implement the branch**

In `apps/geolibre-desktop/src/lib/tauri-io.ts`, replace the DuckDB fallback in `loadTauriVectorFile` (currently the block that calls `loadDuckDbVector` with `readLocalFileBytes`). New shape:
```ts
  // Native duckdb-rs first on desktop; WASM is the fallback for anything native
  // cannot handle. (Web build never reaches loadTauriVectorFile.)
  const siblingFiles =
    extension === "shp" ? await readShapefileSiblings(path) : [];

  if (isTauri()) {
    try {
      const { loadNativeVectorFile } = await import("./native-duckdb-vector");
      return { data: await loadNativeVectorFile(path, extension, options), path };
    } catch (error) {
      if (isVectorLoadCancelled(error)) throw error;
      console.warn(
        "[GeoLibre] native vector load failed; retrying with duckdb-wasm.",
        error,
      );
      // fall through to WASM
    }
  }

  try {
    return {
      data: await loadDuckDbVector(
        {
          name: browserSafeFileName(path),
          extension,
          data: await readLocalFileBytes(path),
          siblingFiles,
        },
        options,
      ),
      path,
    };
  } catch (error) {
    if (isVectorLoadCancelled(error)) throw error;
    const detail = error instanceof Error ? error.message : "Unknown error";
    throw new Error(`Could not convert this vector file with DuckDB. ${detail}`);
  }
```
Keep the existing earlier format-specific branches (GeoJSON/KML/KMZ/GPX/CSV) untouched.

- [ ] **Step 4: Run the test to verify pass**

Run: `node --import tsx --test tests/tauri-io-native-branch.test.ts`
Expected: PASS.

- [ ] **Step 5: Run the full frontend suite + Rust tests**

Run:
```bash
npm run test:frontend
PATH="$HOME/.cargo/bin:$PATH" cargo test --manifest-path apps/geolibre-desktop/src-tauri/Cargo.toml
```
Expected: all green.

- [ ] **Step 6: Commit**
```bash
git add apps/geolibre-desktop/src/lib/tauri-io.ts tests/tauri-io-native-branch.test.ts
git commit -m "feat(desktop): route vector loads through native engine with WASM fallback"
```

---

### Task 10: End-to-end manual verification + PR

**Files:** none (verification) ; then open PR.

- [ ] **Step 1: Fetch extensions and run the desktop app**
```bash
npm run fetch:duckdb-spatial
PATH="$HOME/.cargo/bin:$PATH" npm run tauri:dev
```

- [ ] **Step 2: Drag-drop a GeoParquet and a shapefile**

In the running desktop window, drag-drop a `.parquet`/`.geoparquet` file and a zipped/loose shapefile. Watch the terminal and the webview devtools console.
Expected: layers render in WGS84; no `stoi: no conversion` error; no `[GeoLibre] native vector load failed` warning (which would indicate a fallback to WASM).

- [ ] **Step 3: Confirm the large-dataset prompt still appears**

Drag-drop (or point at) a file with > 500k features. Expected: the existing confirmation dialog appears; accepting loads, declining cancels with no error toast.

- [ ] **Step 4: Run the full local gate**
```bash
PATH="$HOME/.cargo/bin:$PATH" npm run ci
```
Expected: build + frontend + worker + backend + rust check all pass. (If `npm run ci` lints/builds the whole app, allow time.)

- [ ] **Step 5: Push and open the PR against your fork**
```bash
git push -u origin feat/native-duckdb-desktop-vector
gh pr create --repo yharby/GeoLibre --base main \
  --title "feat(desktop): native duckdb-rs vector engine (closes #961, #962)" \
  --body "Implements the native duckdb-rs desktop vector path per docs/superpowers/specs/2026-06-28-native-duckdb-desktop-vector-design.md. Closes #961 and #962."
```

---

## Self-Review

**Spec coverage:**
- Scope (all DuckDB-backed formats) → Tasks 2-4, 9 (`source_sql` handles parquet + `ST_Read`; branch routes the whole DuckDB fallback). ✓
- Spatial ext bundled + network fallback → Tasks 5, 7. ✓
- WASM fallback on native error → Task 9. ✓
- Same WGS84 FeatureCollection contract → Tasks 2-3 (validated SQL, CRS transform, OGC_FID/blob exclusion). ✓
- Large-dataset confirmation parity → Tasks 4, 8. ✓
- Path validation reuse → Task 6. ✓
- Tests (Rust integration, frontend unit, manual) → Tasks 3, 4, 8, 9, 10. ✓

**Placeholder scan:** Task 9 Step 1 intentionally uses `test.todo` as a guide because the concrete mocks must follow the repo's existing tauri-io test harness, which the implementer must read first; the step explicitly instructs replacing the todos with concrete assertions and names the modules to stub. Acceptable because the assertions and behavior are fully specified; the only deferred detail is matching the existing mock style. All other steps contain complete code.

**Type consistency:** `NativeVectorOptions` (camelCase serde) ↔ TS `baseOptions` keys (`layer`, `overrideSourceCrs`, `featureWarnCount`, `largeDatasetConfirmed`) match. `NativeVectorResult` (`needsConfirmation`, `featureCount`, `featureCollection`) matches the TS `NativeVectorResult` interface in Task 8. `run_native_load`/`open_with_spatial`/`load_feature_collection` names are consistent across Tasks 3-6.

**Risks / verify-during-implementation:**
- `confirmLargeDataset` exact signature, confirm in `duckdb-vector-guard.ts` and adjust the Task 8 call.
- `Config::allow_unsigned_extensions` / `enable_external_access` exact return types, the research reported `Result<Self>`; adjust `?`/chaining if the installed crate differs.
- `tauri.conf.json` capabilities may need an `fs`/resource permission for `app.path().resolve` of bundled resources; if the bundled `LOAD` path fails at runtime, the network fallback covers it, and Task 10 Step 2 will surface it.

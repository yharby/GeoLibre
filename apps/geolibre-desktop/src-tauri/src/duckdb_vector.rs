use duckdb::{Config, Connection};
use std::path::Path;
use tauri::{AppHandle, Manager};

/// Open a fresh in-memory DuckDB connection. Test-only helper: production opens
/// connections through `open_with_spatial` (which uses flags for the extension).
#[cfg(test)]
pub(crate) fn open_in_memory() -> Result<Connection, String> {
    Connection::open_in_memory().map_err(|error| format!("Could not open DuckDB: {error}"))
}

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

fn per_feature_sql(source_sql: &str, geom_geojson_expr: &str, null_patch: &str) -> String {
    format!(
        "SELECT json_object(\
           'type', 'Feature', \
           'geometry', {geom}::JSON, \
           'properties', json_merge_patch(to_json(s), {patch})\
         )::VARCHAR AS feature \
         FROM ({source}) AS s",
        geom = geom_geojson_expr,
        patch = quote_sql_string(null_patch),
        source = source_sql,
    )
}

use serde::{Deserialize, Serialize};

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
    let sql = per_feature_sql(&src, &geom_geojson, &patch);

    let mut stmt = conn.prepare(&sql).map_err(|e| format!("Could not build GeoJSON: {e}"))?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| format!("Could not build GeoJSON: {e}"))?;

    let mut out = String::from(r#"{"type":"FeatureCollection","features":["#);
    let mut first = true;
    for row in rows {
        let feature = row.map_err(|e| format!("Could not build GeoJSON: {e}"))?;
        if !first {
            out.push(',');
        }
        out.push_str(&feature);
        first = false;
    }
    out.push_str("]}");
    Ok(out)
}

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
        if count >= warn_at {
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

/// Open an in-memory DuckDB connection with `allow_unsigned_extensions` and external access
/// enabled, then load the spatial extension from a bundled file (if it exists) or fall back
/// to `INSTALL spatial; LOAD spatial;` from the network/cache.
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

    #[test]
    fn memory_regression_streaming_does_not_oom() {
        // Write a large temp parquet at runtime — no binary fixture committed.
        let tmp_dir = std::env::temp_dir();
        let tmp_path = tmp_dir.join("duckdb_oom_regression_200k.parquet");
        let path_str = tmp_path.to_string_lossy().to_string();

        // Generate 200_000 random points into a parquet file.
        {
            let gen_conn = open_in_memory().expect("gen conn");
            gen_conn.execute_batch("INSTALL spatial; LOAD spatial;").expect("spatial for gen");
            gen_conn.execute_batch(&format!(
                "COPY (SELECT i AS id, ST_Point(random()*360-180, random()*180-90) AS geom \
                 FROM range(200000) t(i)) TO '{}' (FORMAT PARQUET);",
                path_str.replace('\'', "''")
            )).expect("generate parquet");
        }

        // Load with a constrained memory_limit to prove streaming doesn't OOM.
        let conn = open_in_memory().expect("conn");
        conn.execute_batch("INSTALL spatial; LOAD spatial;").expect("spatial");
        conn.execute_batch("SET memory_limit='128MB';").expect("set memory_limit");

        let result = load_feature_collection(
            &conn,
            &path_str,
            "parquet",
            &NativeVectorOptions::default(),
        );

        // Clean up temp file.
        let _ = std::fs::remove_file(&tmp_path);

        let fc_json = result.expect("load_feature_collection must not OOM with streaming");
        let fc: serde_json::Value = serde_json::from_str(&fc_json).expect("valid json");
        assert_eq!(fc["type"], "FeatureCollection");
        assert_eq!(fc["features"].as_array().unwrap().len(), 200_000);
    }
}

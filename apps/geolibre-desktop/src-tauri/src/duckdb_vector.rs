use duckdb::Connection;

/// Open a fresh in-memory DuckDB connection for a single vector load.
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
}

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::{Map, Value};

#[derive(Debug, Clone, Deserialize)]
pub struct IndexMapping {
    pub mappings: Mappings,
}

/// Body of one index in a `_mapping` response.
///
/// ES >= 7 puts `properties` directly under `mappings`. ES <= 6 nests it one
/// level deeper, under each mapping type name (`mappings.<type>.properties`),
/// alongside type-level metadata keys such as `_all` and `_source`. The raw
/// map keeps both shapes readable; use [`Mappings::properties`].
#[derive(Debug, Clone, Deserialize)]
pub struct Mappings {
    #[serde(flatten)]
    raw: HashMap<String, Value>,
}

impl Mappings {
    /// Field mappings, merged across the mapping types of an ES <= 6 index.
    pub fn properties(&self) -> HashMap<String, FieldMapping> {
        fn parse(value: &Value) -> HashMap<String, FieldMapping> {
            serde_json::from_value(value.clone()).unwrap_or_default()
        }

        match self.raw.get("properties") {
            Some(properties) => parse(properties),
            None => self
                .raw
                .values()
                .filter_map(|type_mapping| type_mapping.get("properties"))
                .flat_map(parse)
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct FieldMapping {
    #[serde(rename = "type")]
    pub data_type: Option<String>,

    pub index: Option<bool>,
    pub analyzer: Option<String>,
    pub search_analyzer: Option<String>,
    pub ignore_above: Option<usize>,

    pub properties: Option<HashMap<String, FieldMapping>>,
    pub fields: Option<HashMap<String, FieldMapping>>,
}

/// One row of a `_cat/indices` response.
///
/// Only the name is read. The remaining columns vary by server version and by
/// index state — `dataset.size` arrived in ES 8, and a closed index on ES <= 6
/// reports `health`, `pri`, `rep`, `docs.*` and `store.*` as `null` — so
/// binding them here only creates decode failures.
#[derive(Debug, Clone, Deserialize)]
pub struct Index {
    pub index: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Column {
    pub name: String,
    #[serde(rename = "type")]
    pub data_type: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Field {
    pub field: String,
    pub format: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SqlResponse {
    pub columns: Vec<Column>,
    pub rows: Vec<Vec<Value>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EsqlResponse {
    pub took: usize,
    pub is_partial: bool,
    pub documents_found: usize,
    pub values_loaded: usize,
    pub columns: Vec<Column>,
    pub values: Vec<Vec<Value>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Shards {
    pub total: i64,
    pub successful: i64,
    pub skipped: i64,
    pub failed: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Hit {
    #[serde(rename = "_index")]
    pub index: String,
    #[serde(rename = "_id")]
    pub id: String,
    #[serde(rename = "_score")]
    pub score: Option<f64>,
    #[serde(rename = "_source")]
    pub source: Option<Map<String, Value>>,
    pub fields: Option<Map<String, Value>>,
    pub sort: Option<Vec<Value>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Hits {
    pub max_score: Option<f64>,
    pub hits: Vec<Hit>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SearchResponse {
    pub took: usize,
    pub timed_out: bool,

    #[serde(rename = "_shards")]
    pub shards: Shards,
    pub hits: Hits,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `_cat/indices` rows decode across server versions and index states.
    /// Cases: an ES 5.6 closed index (every column but the name is `null`,
    /// `dataset.size` absent), an ES 5.6 open index (`dataset.size` still
    /// absent), and an ES 9 open index (every column present).
    #[test]
    fn cat_indices_rows_decode() {
        let cases = [
            (
                "es5 closed",
                r#"[{"health":null,"status":"close","index":".elastichq","uuid":"5m0hV2JWQVylclKlQEaBcQ","pri":null,"rep":null,"docs.count":null,"docs.deleted":null,"store.size":null,"pri.store.size":null}]"#,
                ".elastichq",
            ),
            (
                "es5 open",
                r#"[{"health":"green","status":"open","index":"log-concentrated-icms-2026.01","uuid":"n8mufRfTQ8-QK_J5cPy_fw","pri":"2","rep":"1","docs.count":"518594","docs.deleted":"0","store.size":"1gb","pri.store.size":"512mb"}]"#,
                "log-concentrated-icms-2026.01",
            ),
            (
                "es9 open",
                r#"[{"health":"yellow","status":"open","index":"posts","uuid":"qF43thqoREGPmSAffkU-Zw","pri":"1","rep":"1","docs.count":"5","docs.deleted":"0","store.size":"11kb","pri.store.size":"11kb","dataset.size":"11kb"}]"#,
                "posts",
            ),
        ];

        for (name, body, want) in cases {
            let got: Vec<Index> = serde_json::from_str(body).expect(name);
            assert_eq!(got.len(), 1, "{name}");
            assert_eq!(got[0].index, want, "{name}");
        }
    }

    /// `Mappings::properties` finds the field mappings in both response
    /// layouts: ES <= 6 nests them under each mapping type name and mixes in
    /// type-level metadata keys; ES >= 7 puts them directly under `mappings`.
    #[test]
    fn mapping_properties_decode() {
        let cases = [
            (
                "es5 typed mapping",
                r#"{"mappings":{"logs":{"_all":{"enabled":false},"properties":{"@timestamp":{"type":"date"},"ac_power_meter_freq":{"type":"float","index":false}}}}}"#,
                vec!["@timestamp", "ac_power_meter_freq"],
            ),
            (
                "es5 multiple types merged",
                r#"{"mappings":{"logs":{"properties":{"@timestamp":{"type":"date"}}},"events":{"properties":{"author":{"type":"text"}}}}}"#,
                vec!["@timestamp", "author"],
            ),
            (
                "es9 typeless mapping",
                r#"{"mappings":{"_meta":{"created":1},"properties":{"author":{"type":"text","fields":{"keyword":{"type":"keyword","ignore_above":256}}},"content":{"type":"text"}}}}"#,
                vec!["author", "content"],
            ),
            ("index with no fields", r#"{"mappings":{}}"#, vec![]),
        ];

        for (name, body, want) in cases {
            let got: IndexMapping = serde_json::from_str(body).expect(name);
            let mut names: Vec<String> = got.mappings.properties().into_keys().collect();
            names.sort();
            assert_eq!(names, want, "{name}");
        }
    }
}

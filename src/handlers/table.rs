//! Lay out search results as a table: `tabulate` for rows of named cells,
//! `from_aggregations` for the `aggregations` of a search response.
//!
//! In `from_aggregations`, each innermost bucket becomes a row. A row holds the key of every bucket
//! on its path, outer to inner, followed by the metrics found along that
//! path. This is the shape the NLPchina `_sql` plugin produces for
//! `GROUP BY a, b` (one `terms` aggregation per grouping field, metrics in
//! the innermost bucket), and it reads naturally for hand-written `aggs`.

use serde_json::{Map, Value};
use std::collections::HashMap;

/// Ordered `(column, value)` cells of one row.
pub type Cells = Vec<(String, Value)>;

#[derive(Clone, Default)]
struct Row {
    keys: Cells,
    metrics: Cells,
}

pub fn from_aggregations(aggregations: &Map<String, Value>) -> (Vec<String>, Vec<Vec<Value>>) {
    let mut rows = Vec::new();
    expand(aggregations, Row::default(), &mut rows);

    tabulate(
        rows.into_iter()
            .map(|row| row.keys.into_iter().chain(row.metrics).collect())
            .collect(),
    )
}

/// Lay out rows of named cells under the union of their column names, in
/// first-seen order. A row without a column gets null there.
pub fn tabulate(rows: Vec<Cells>) -> (Vec<String>, Vec<Vec<Value>>) {
    let mut columns: Vec<String> = Vec::new();
    let mut position: HashMap<String, usize> = HashMap::new();
    for (name, _) in rows.iter().flatten() {
        if !position.contains_key(name) {
            position.insert(name.clone(), columns.len());
            columns.push(name.clone());
        }
    }

    let rows = rows
        .into_iter()
        .map(|cells| {
            let mut row = vec![Value::Null; columns.len()];
            for (name, value) in cells {
                row[position[&name]] = value;
            }
            row
        })
        .collect();

    (columns, rows)
}

/// Add the rows below one level of aggregations to `out`.
///
/// A level groups by its bucket aggregation when it has exactly one. With
/// several sibling bucket aggregations there is no single way to combine
/// them into rows, so each is shown as a JSON cell instead.
fn expand(aggs: &Map<String, Value>, mut row: Row, out: &mut Vec<Row>) {
    let grouping_count = aggs.values().filter(|agg| buckets(agg).is_some()).count();
    let mut grouping = None;

    for (name, agg) in aggs {
        match buckets(agg) {
            Some(b) if grouping_count == 1 => grouping = Some((name, b)),
            _ => push_metric(name, agg, &mut row.metrics),
        }
    }

    let Some((name, buckets)) = grouping else {
        out.push(row);
        return;
    };

    for (key, bucket) in buckets {
        let mut bucket_row = row.clone();
        bucket_row.keys.push((name.clone(), key));

        let sub_aggs: Map<String, Value> = bucket
            .iter()
            .filter(|(_, v)| v.is_object())
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        if sub_aggs.is_empty() {
            let doc_count = bucket.get("doc_count").cloned().unwrap_or_default();
            bucket_row
                .metrics
                .push(("doc_count".to_string(), doc_count));
            out.push(bucket_row);
        } else {
            expand(&sub_aggs, bucket_row, out);
        }
    }
}

/// The `(key, bucket)` pairs of a bucket aggregation, or `None` for a metric.
/// Buckets come as an array, or as an object keyed by bucket name when the
/// request set `keyed`.
fn buckets(agg: &Value) -> Option<Vec<(Value, &Map<String, Value>)>> {
    match agg.get("buckets")? {
        Value::Array(items) => Some(
            items
                .iter()
                .filter_map(Value::as_object)
                .map(|b| {
                    let key = b.get("key_as_string").or(b.get("key"));
                    (key.cloned().unwrap_or_default(), b)
                })
                .collect(),
        ),
        Value::Object(items) => Some(
            items
                .iter()
                .filter_map(|(k, b)| b.as_object().map(|b| (Value::String(k.clone()), b)))
                .collect(),
        ),
        _ => None,
    }
}

/// A single-value metric is one cell, preferring its formatted
/// `value_as_string` (dates). A multi-value metric whose fields are all
/// scalars (`stats`, `percentiles` values) is one cell per field. Anything
/// else is kept whole as a JSON cell.
fn push_metric(name: &str, agg: &Value, cells: &mut Cells) {
    if let Some(value) = agg.get("value") {
        let value = agg.get("value_as_string").unwrap_or(value);
        cells.push((name.to_string(), value.clone()));
        return;
    }

    let fields = agg
        .get("values")
        .and_then(Value::as_object)
        .or(agg.as_object())
        .filter(|fields| !fields.values().any(|v| v.is_object() || v.is_array()));

    match fields {
        Some(fields) => {
            for (field, value) in fields {
                cells.push((format!("{name}.{field}"), value.clone()));
            }
        }
        None => cells.push((name.to_string(), agg.clone())),
    }
}

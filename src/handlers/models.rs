use crate::es::models::{EsqlResponse, FieldMapping, SearchResponse, SqlResponse};
use crate::handlers::table;
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum QueryMode {
    None,
    Sql,
    Rest,
    Esql,
}

#[derive(Debug, Clone)]
pub struct Query {
    pub mode: QueryMode,
    pub body: String,
}

impl From<String> for Query {
    fn from(query: String) -> Self {
        let query = query.trim_start();

        let (header, body) = query.split_once('\n').unwrap_or((query, ""));

        let (mode, query) = match header.trim() {
            "#!sql" => (QueryMode::Sql, body.trim_start()),
            "#!rest" => (QueryMode::Rest, body.trim_start()),
            "#!esql" => (QueryMode::Esql, body.trim_start()),
            _ => (QueryMode::None, query),
        };

        Self {
            mode,
            body: query.to_owned(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ColumnResponse {
    pub name: String,
    pub data_type: String,
    pub is_pk: bool,
    pub is_nullable: bool,
    pub is_auto_increment: bool,
}

impl ColumnResponse {
    pub fn from_values(name: String, mapping: FieldMapping) -> Self {
        let mut data_type = if let Some(dt) = mapping.data_type {
            dt
        } else if mapping.properties.is_some() {
            "object".to_string()
        } else {
            "unknown".to_string()
        };

        if mapping
            .fields
            .as_ref()
            .is_some_and(|f| f.contains_key("keyword"))
        {
            data_type.push_str(",keyword");
        }

        Self {
            name,
            data_type,
            is_pk: false,
            is_nullable: false,
            is_auto_increment: false,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ExecuteQueryResponse {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
    pub affected_rows: usize,
    pub execution_time_ms: usize,
    /// More rows exist than were returned.
    pub truncated: bool,
}

impl ExecuteQueryResponse {
    /// Keep at most `limit` rows.
    pub fn capped(mut self, limit: Option<usize>) -> Self {
        if let Some(limit) = limit.filter(|&l| self.rows.len() > l) {
            self.rows.truncate(limit);
            self.truncated = true;
        }
        self.affected_rows = self.rows.len();
        self
    }
}

impl From<SqlResponse> for ExecuteQueryResponse {
    fn from(t: SqlResponse) -> Self {
        let affected_rows = t.rows.len();

        Self {
            columns: t.columns.iter().map(|c| c.name.clone()).collect(),
            rows: t.rows,
            affected_rows,
            execution_time_ms: 0,
            truncated: t.cursor.is_some(),
        }
    }
}

impl From<EsqlResponse> for ExecuteQueryResponse {
    fn from(t: EsqlResponse) -> Self {
        Self {
            columns: t.columns.iter().map(|c| c.name.clone()).collect(),
            rows: t.values,
            affected_rows: t.documents_found,
            execution_time_ms: t.took,
            truncated: false,
        }
    }
}

/// A response carrying `aggregations` is shown as its aggregation table — an
/// aggregation query asks for the buckets, and its hits (if any) are only a
/// side effect of the request's `size`. Otherwise each hit is a row.
impl From<SearchResponse> for ExecuteQueryResponse {
    fn from(resp: SearchResponse) -> Self {
        let (columns, rows) = match resp.aggregations {
            Some(aggs) if !aggs.is_empty() => table::from_aggregations(&aggs),
            _ => table::tabulate(
                resp.hits
                    .hits
                    .into_iter()
                    .map(|hit| {
                        hit.source
                            .or(hit.fields)
                            .map(|m| m.into_iter().collect())
                            .unwrap_or_default()
                    })
                    .collect(),
            ),
        };

        Self {
            affected_rows: rows.len(),
            execution_time_ms: resp.took,
            columns,
            rows,
            truncated: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[derive(Debug)]
    struct TestCase<'a> {
        name: &'a str,
        input: &'a str,
        exp_mode: QueryMode,
        exp_query: &'a str,
    }

    #[test]
    fn parse_query() {
        let cases = [
            TestCase {
                name: "sql",
                input: "#!sql\nSELECT * FROM users",
                exp_mode: QueryMode::Sql,
                exp_query: "SELECT * FROM users",
            },
            TestCase {
                name: "rest",
                input: "#!rest\nGET /users/_search",
                exp_mode: QueryMode::Rest,
                exp_query: "GET /users/_search",
            },
            TestCase {
                name: "esql",
                input: "#!esql\nFROM users",
                exp_mode: QueryMode::Esql,
                exp_query: "FROM users",
            },
            TestCase {
                name: "default",
                input: "SELECT * FROM users",
                exp_mode: QueryMode::None,
                exp_query: "SELECT * FROM users",
            },
        ];

        for case in cases {
            let parsed = Query::from(case.input.to_owned());

            assert_eq!(parsed.mode, case.exp_mode, "{}", case.name);
            assert_eq!(parsed.body, case.exp_query, "{}", case.name);
        }
    }

    fn table(body: &str) -> (Vec<String>, Vec<Vec<Value>>) {
        let resp: SearchResponse = serde_json::from_str(body).expect("search response");
        let got = ExecuteQueryResponse::from(resp);
        (got.columns, got.rows)
    }

    /// A search response with `aggregations` becomes one row per innermost
    /// bucket: the bucket keys first, outer to inner, then the metrics. The
    /// bodies are shaped like what the NLPchina `_sql` plugin returns on
    /// ES 5.6 and what `#!rest` returns for an `aggs` request.
    /// Cases: a bare `COUNT(*)`, `GROUP BY` one field, `GROUP BY` two fields
    /// with two metrics (one of them null), `GROUP BY` with no metric (the
    /// bucket's `doc_count` stands in), metrics that carry `value_as_string`,
    /// a multi-value `stats` metric, `keyed` buckets, a metric sitting beside
    /// a grouping, and a grouping that matched nothing.
    #[test]
    fn aggregations_become_rows() {
        let cases: [(&str, &str, Vec<&str>, Value); 9] = [
            (
                "count",
                r#"{"took":1,"timed_out":false,"_shards":{"total":1,"successful":1,"skipped":0,"failed":0},"hits":{"total":42,"max_score":0.0,"hits":[]},"aggregations":{"COUNT(*)":{"value":42}}}"#,
                vec!["COUNT(*)"],
                json!([[42]]),
            ),
            (
                "group by one field",
                r#"{"took":1,"timed_out":false,"_shards":{"total":1,"successful":1,"skipped":0,"failed":0},"hits":{"total":5,"max_score":0.0,"hits":[]},"aggregations":{"deviceType":{"doc_count_error_upper_bound":0,"sum_other_doc_count":0,"buckets":[{"key":"STRINGMETER","doc_count":3,"COUNT(*)":{"value":3}},{"key":"INVERTER","doc_count":2,"COUNT(*)":{"value":2}}]}}}"#,
                vec!["deviceType", "COUNT(*)"],
                json!([["STRINGMETER", 3], ["INVERTER", 2]]),
            ),
            (
                "group by two fields",
                r#"{"took":1,"timed_out":false,"_shards":{"total":1,"successful":1,"skipped":0,"failed":0},"hits":{"total":4,"max_score":0.0,"hits":[]},"aggregations":{"plantNo":{"buckets":[{"key":"P1","doc_count":3,"deviceType":{"buckets":[{"key":"STRINGMETER","doc_count":2,"MAX(kw)":{"value":null},"n":{"value":2}},{"key":"INVERTER","doc_count":1,"MAX(kw)":{"value":142.5},"n":{"value":1}}]}},{"key":"P2","doc_count":1,"deviceType":{"buckets":[{"key":"INVERTER","doc_count":1,"MAX(kw)":{"value":7.0},"n":{"value":1}}]}}]}}}"#,
                vec!["plantNo", "deviceType", "MAX(kw)", "n"],
                json!([
                    ["P1", "STRINGMETER", null, 2],
                    ["P1", "INVERTER", 142.5, 1],
                    ["P2", "INVERTER", 7.0, 1]
                ]),
            ),
            (
                "group by without metric",
                r#"{"took":1,"timed_out":false,"_shards":{"total":1,"successful":1,"skipped":0,"failed":0},"hits":{"total":5,"max_score":0.0,"hits":[]},"aggregations":{"deviceType":{"buckets":[{"key":"STRINGMETER","doc_count":3},{"key":"INVERTER","doc_count":2}]}}}"#,
                vec!["deviceType", "doc_count"],
                json!([["STRINGMETER", 3], ["INVERTER", 2]]),
            ),
            (
                "value_as_string and date histogram key",
                r#"{"took":1,"timed_out":false,"_shards":{"total":1,"successful":1,"skipped":0,"failed":0},"hits":{"total":5,"max_score":0.0,"hits":[]},"aggregations":{"day":{"buckets":[{"key":1790812800000,"key_as_string":"2026-10-01T00:00:00.000Z","doc_count":5,"MIN(ts)":{"value":1.7908128E12,"value_as_string":"2026-10-01T00:00:00.000Z"}}]}}}"#,
                vec!["day", "MIN(ts)"],
                json!([["2026-10-01T00:00:00.000Z", "2026-10-01T00:00:00.000Z"]]),
            ),
            (
                "stats metric",
                r#"{"took":1,"timed_out":false,"_shards":{"total":1,"successful":1,"skipped":0,"failed":0},"hits":{"total":2,"max_score":0.0,"hits":[]},"aggregations":{"kw":{"count":2,"min":1.0,"max":3.0,"avg":2.0,"sum":4.0}}}"#,
                vec!["kw.avg", "kw.count", "kw.max", "kw.min", "kw.sum"],
                json!([[2.0, 2, 3.0, 1.0, 4.0]]),
            ),
            (
                "keyed buckets",
                r#"{"took":1,"timed_out":false,"_shards":{"total":1,"successful":1,"skipped":0,"failed":0},"hits":{"total":5,"max_score":0.0,"hits":[]},"aggregations":{"band":{"buckets":{"high":{"doc_count":2},"low":{"doc_count":3}}}}}"#,
                vec!["band", "doc_count"],
                json!([["high", 2], ["low", 3]]),
            ),
            (
                "metric beside a grouping",
                r#"{"took":1,"timed_out":false,"_shards":{"total":1,"successful":1,"skipped":0,"failed":0},"hits":{"total":5,"max_score":0.0,"hits":[]},"aggregations":{"total":{"value":5},"deviceType":{"buckets":[{"key":"INVERTER","doc_count":2,"avg":{"value":1.5}}]}}}"#,
                vec!["deviceType", "total", "avg"],
                json!([["INVERTER", 5, 1.5]]),
            ),
            (
                "grouping that matched nothing",
                r#"{"took":1,"timed_out":false,"_shards":{"total":1,"successful":1,"skipped":0,"failed":0},"hits":{"total":0,"max_score":0.0,"hits":[]},"aggregations":{"deviceType":{"buckets":[]}}}"#,
                vec![],
                json!([]),
            ),
        ];

        for (name, body, want_columns, want_rows) in cases {
            let (columns, rows) = table(body);
            assert_eq!(columns, want_columns, "{name}");
            assert_eq!(json!(rows), want_rows, "{name}");
        }
    }

    /// `capped` keeps at most `limit` rows and marks the result truncated
    /// when it dropped any, or when the server already said more rows exist.
    /// Cases: over the limit, under it, exactly at it, no limit, and a result
    /// the server already marked truncated.
    #[test]
    fn capped_keeps_at_most_limit_rows() {
        let cases = [
            ("over", 3, false, Some(2), 2, true),
            ("under", 3, false, Some(5), 3, false),
            ("exact", 3, false, Some(3), 3, false),
            ("no limit", 3, false, None, 3, false),
            ("server truncated", 2, true, Some(5), 2, true),
        ];

        for (name, rows, truncated, limit, want_rows, want_truncated) in cases {
            let resp = ExecuteQueryResponse {
                columns: vec!["a".to_string()],
                rows: (0..rows).map(|i| vec![json!(i)]).collect(),
                affected_rows: rows,
                execution_time_ms: 0,
                truncated,
            }
            .capped(limit);

            assert_eq!(resp.rows.len(), want_rows, "{name}");
            assert_eq!(resp.affected_rows, want_rows, "{name}");
            assert_eq!(resp.truncated, want_truncated, "{name}");
        }
    }

    /// Hits become rows whose columns cover every field seen in any hit, not
    /// only the first one; a hit that lacks a field gets null there.
    #[test]
    fn hits_columns_cover_every_hit() {
        let body = r#"{"took":1,"timed_out":false,"_shards":{"total":1,"successful":1,"skipped":0,"failed":0},"hits":{"total":2,"max_score":1.0,"hits":[{"_index":"i","_id":"1","_score":1.0,"_source":{"a":1}},{"_index":"i","_id":"2","_score":1.0,"_source":{"a":2,"b":"x"}}]}}"#;

        let (columns, rows) = table(body);

        assert_eq!(columns, vec!["a", "b"]);
        assert_eq!(json!(rows), json!([[1, null], [2, "x"]]));
    }
}

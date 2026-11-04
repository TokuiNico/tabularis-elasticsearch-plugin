use serde_json::Value;

/// Extracts the URL from the params object
pub fn extract_url(params: &Value) -> Option<String> {
    params
        .get("params")
        .and_then(|p| p.get("database")) // Because we use database field for storing the URL
        .and_then(|d| d.as_str())
        .map(|s| s.to_string())
}

/// Extracts the table from the params object
pub fn extract_table(params: &Value) -> Option<String> {
    params
        .get("table")
        .and_then(|t| t.as_str())
        .map(|s| s.to_string())
}

/// Extracts the row cap Tabularis sends with `execute_query`. Zero counts as
/// no cap.
pub fn extract_limit(params: &Value) -> Option<usize> {
    params
        .get("limit")
        .and_then(Value::as_u64)
        .filter(|&n| n > 0)
        .map(|n| n as usize)
}

/// Extracts the "query" from the params object
pub fn extract_query(params: &Value) -> Option<String> {
    params
        .get("query")
        .and_then(|d| d.as_str())
        .map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::extract_limit;
    use serde_json::json;

    /// `limit` is the row cap Tabularis sends with `execute_query` (the MCP
    /// `run_query` limit, or the editor's page size). Zero, null and a
    /// missing value all mean no cap.
    #[test]
    fn limit_is_positive_or_absent() {
        let cases = [
            ("set", json!({"limit": 100}), Some(100)),
            ("zero", json!({"limit": 0}), None),
            ("null", json!({"limit": null}), None),
            ("missing", json!({}), None),
        ];

        for (name, params, want) in cases {
            assert_eq!(extract_limit(&params), want, "{name}");
        }
    }
}

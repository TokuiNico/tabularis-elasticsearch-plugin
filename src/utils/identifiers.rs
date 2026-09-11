//! SQL identifier quoting.

/// Quote an identifier using the given quote character, doubling any
/// occurrences of that character inside the identifier to escape them.
///
/// ```
/// assert_eq!(quote_identifier("users", '"'), "\"users\"");
/// assert_eq!(quote_identifier("weird\"name", '"'), "\"weird\"\"name\"");
/// assert_eq!(quote_identifier("items", '`'),  "`items`");
/// ```
pub fn quote_identifier(name: &str, quote: char) -> String {
    let mut out = String::with_capacity(name.len() + 2);
    out.push(quote);
    for c in name.chars() {
        if c == quote {
            out.push(quote);
        }
        out.push(c);
    }
    out.push(quote);
    out
}

/// Strip the quoting from the table name in a `FROM` clause.
///
/// The NLPchina `_sql` plugin takes a quoted index name literally and then
/// reports it as missing, so the name Tabularis quotes has to be handed over
/// bare. Only the token after `FROM` is touched: the same plugin does accept
/// `"` as a string delimiter, and whitespace is left exactly as written so
/// that literals keep their spacing.
pub fn unquote_from_target(statement: &str) -> String {
    let mut out = String::with_capacity(statement.len());
    let mut rest = statement;
    let mut after_from = false;

    while !rest.is_empty() {
        let gap = rest
            .find(|c: char| !c.is_whitespace())
            .unwrap_or(rest.len());
        out.push_str(&rest[..gap]);
        rest = &rest[gap..];

        if rest.is_empty() {
            break;
        }

        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let token = &rest[..end];
        rest = &rest[end..];

        if after_from {
            out.push_str(&token.replace(['"', '`'], ""));
        } else {
            out.push_str(token);
        }
        after_from = token.eq_ignore_ascii_case("from");
    }

    out
}

#[cfg(test)]
mod tests {
    use super::{quote_identifier, unquote_from_target};

    #[test]
    fn quotes_plain_names() {
        assert_eq!(quote_identifier("users", '"'), "\"users\"");
        assert_eq!(quote_identifier("users", '`'), "`users`");
    }

    #[test]
    fn escapes_embedded_quotes() {
        assert_eq!(quote_identifier("a\"b", '"'), "\"a\"\"b\"");
        assert_eq!(quote_identifier("a`b", '`'), "`a``b`");
    }

    #[test]
    fn handles_empty() {
        assert_eq!(quote_identifier("", '"'), "\"\"");
    }

    /// Only the `FROM` target loses its quoting. Cases: the statement
    /// Tabularis generates for a clicked index, a quoted string literal in a
    /// `WHERE` clause, the spacing of such a literal, a lowercase keyword, a
    /// backtick-quoted name, and a statement with no `FROM` at all.
    #[test]
    fn unquotes_only_the_from_target() {
        let cases = [
            (
                r#"SELECT * FROM "log-concentrated-icms-2026.09""#,
                "SELECT * FROM log-concentrated-icms-2026.09",
            ),
            (
                r#"SELECT * FROM "posts" WHERE author = "Erwin""#,
                r#"SELECT * FROM posts WHERE author = "Erwin""#,
            ),
            (
                r#"SELECT * FROM "posts" WHERE title = 'a  b'"#,
                r#"SELECT * FROM posts WHERE title = 'a  b'"#,
            ),
            (r#"select * from "posts""#, "select * from posts"),
            (r#"SELECT * FROM `posts`"#, "SELECT * FROM posts"),
            ("SELECT 1", "SELECT 1"),
        ];

        for (statement, want) in cases {
            assert_eq!(unquote_from_target(statement), want, "{statement}");
        }
    }
}

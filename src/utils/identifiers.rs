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

/// Strip the quoting Tabularis puts around identifiers.
///
/// The NLPchina `_sql` plugin reads a quoted name as a string literal, so a
/// quoted index name comes back as missing and a quoted column silently
/// compares two literals and matches nothing. Tabularis reserves `"` for
/// identifiers (`.tabularium` sets `identifier_quote`) and writes string
/// literals with `'`, so every `"` outside a `'…'` literal is dropped; text
/// inside a literal is left exactly as written.
pub fn strip_identifier_quotes(statement: &str) -> String {
    let mut out = String::with_capacity(statement.len());
    let mut in_literal = false;

    for c in statement.chars() {
        match c {
            '\'' => {
                in_literal = !in_literal;
                out.push(c);
            }
            '"' | '`' if !in_literal => {}
            _ => out.push(c),
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::{quote_identifier, strip_identifier_quotes};

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

    /// Identifiers lose their quoting, `'…'` literals keep every character.
    /// Cases: the statement Tabularis generates for a clicked index, a
    /// filtered and ordered query, a literal holding double quotes, a literal
    /// holding its own spacing, backticks, and a statement with nothing to
    /// strip.
    #[test]
    fn strips_identifier_quotes_outside_literals() {
        let cases = [
            (
                r#"SELECT * FROM "log-concentrated-thailand-2026.09""#,
                "SELECT * FROM log-concentrated-thailand-2026.09",
            ),
            (
                r#"SELECT * FROM "posts" WHERE "plantNo" = 'SIM009' ORDER BY "created_at" DESC LIMIT 500"#,
                "SELECT * FROM posts WHERE plantNo = 'SIM009' ORDER BY created_at DESC LIMIT 500",
            ),
            (
                r#"SELECT * FROM "posts" WHERE "title" = 'say "hi"'"#,
                r#"SELECT * FROM posts WHERE title = 'say "hi"'"#,
            ),
            (
                r#"SELECT * FROM "posts" WHERE "title" = 'a  b'"#,
                r#"SELECT * FROM posts WHERE title = 'a  b'"#,
            ),
            (r#"SELECT * FROM `posts`"#, "SELECT * FROM posts"),
            ("SELECT 1", "SELECT 1"),
        ];

        for (statement, want) in cases {
            assert_eq!(strip_identifier_quotes(statement), want, "{statement}");
        }
    }
}

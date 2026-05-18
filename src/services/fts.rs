/// Build an FTS5 query from natural language: split into tokens, escape quotes, join with OR.
pub fn build_fts_query(query: &str) -> String {
    let tokens: Vec<String> = query
        .split_whitespace()
        .filter(|token| token.len() >= 2)
        .map(|token| {
            let escaped = token.replace('"', "\"\"");
            format!("\"{escaped}\"")
        })
        .collect();
    if tokens.is_empty() {
        return format!("\"{}\"", query.replace('"', "\"\""));
    }
    tokens.join(" OR ")
}

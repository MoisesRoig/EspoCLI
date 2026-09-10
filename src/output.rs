//! TSV rendering. stdout carries data only; counters and diagnostics go to stderr.

use serde_json::{Map, Value};
use std::io::Write;

/// Record values are untrusted: tabs and newlines would break the row, and an ESC would
/// let a CRM field drive the terminal. Both are neutralised here, accents are kept.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\t' => out.push(' '),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            // Cc covers C0, DEL and C1, which is every ANSI/OSC introducer.
            c if c.is_control() => {}
            // Bidi overrides can reorder displayed text without changing the bytes.
            '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' => {}
            _ => out.push(c),
        }
    }
    out
}

pub fn cell(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => escape(s),
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Array(a)) => {
            a.iter().map(|x| cell(Some(x))).collect::<Vec<_>>().join(",")
        }
        Some(other) => escape(&other.to_string()),
    }
}

/// Column order comes from the caller; sorted keys of the first row when no selection was made.
pub fn columns(rows: &[Value], selected: &[String]) -> Vec<String> {
    if !selected.is_empty() {
        return selected.to_vec();
    }
    rows.iter()
        .find_map(|r| r.as_object())
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default()
}

/// A report names a related column `link.field`, but the row carries it flattened as
/// `link_fieldName`; a plain link column `x` arrives as `xName`. Resolved per row because
/// Espo drops the `Name` key on a row whose link is empty.
fn row_value<'a>(row: &'a Map<String, Value>, col: &str) -> Option<&'a Value> {
    if let Some(v) = row.get(col) {
        return Some(v);
    }
    let flat = col.replace('.', "_");
    row.get(&format!("{flat}Name"))
        .or_else(|| row.get(&flat))
        .or_else(|| row.get(&format!("{flat}Id")))
}

pub fn print_rows(rows: &[Value], cols: &[String], header: bool) {
    let out = std::io::stdout();
    let mut w = std::io::BufWriter::new(out.lock());
    if header && !cols.is_empty() {
        let _ = writeln!(w, "{}", cols.join("\t"));
    }
    for row in rows {
        let line: Vec<String> = match row.as_object() {
            Some(obj) => cols.iter().map(|c| cell(row_value(obj, c))).collect(),
            None => vec![cell(Some(row))],
        };
        let _ = writeln!(w, "{}", line.join("\t"));
    }
}

/// One `field<TAB>value` line per attribute; no header, a single record needs no column names.
pub fn print_record(obj: &Map<String, Value>, cols: &[String]) {
    let out = std::io::stdout();
    let mut w = std::io::BufWriter::new(out.lock());
    if cols.is_empty() {
        for (k, v) in obj {
            let _ = writeln!(w, "{k}\t{}", cell(Some(v)));
        }
    } else {
        for k in cols {
            let _ = writeln!(w, "{k}\t{}", cell(obj.get(k)));
        }
    }
}

pub fn print_json(v: &Value) {
    println!("{v}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn escaping_keeps_rows_parseable() {
        assert_eq!(escape("a\tb"), "a b");
        assert_eq!(escape("a\nb"), "a\\nb");
        assert_eq!(escape("a\r\nb"), "a\\nb");
        assert_eq!(escape("plain"), "plain");
        assert_eq!(escape("acentós ok"), "acentós ok");
        assert_eq!(escape("日本語 ok"), "日本語 ok");
    }

    #[test]
    fn escaping_neutralises_terminal_control_sequences() {
        assert_eq!(escape("\u{1b}[31mRED\u{1b}[0m"), "[31mRED[0m");
        assert_eq!(escape("title\u{1b}]0;pwned\u{7}"), "title]0;pwned");
        assert_eq!(escape("a\u{7f}b"), "ab");
        assert_eq!(escape("a\u{9b}b"), "ab");
        assert_eq!(escape("user\u{202e}gnp.exe"), "usergnp.exe");
        // A value can no longer contain anything that moves the cursor or ends the row.
        let hostile = "\u{1b}[2J\u{1b}[H\tcol\nrow";
        let clean = escape(hostile);
        assert!(!clean.contains('\u{1b}') && !clean.contains('\t') && !clean.contains('\n'));
    }

    #[test]
    fn cells_render_every_json_shape() {
        assert_eq!(cell(None), "");
        assert_eq!(cell(Some(&json!(null))), "");
        assert_eq!(cell(Some(&json!(true))), "true");
        assert_eq!(cell(Some(&json!(3))), "3");
        assert_eq!(cell(Some(&json!("x"))), "x");
        assert_eq!(cell(Some(&json!(["a", "b"]))), "a,b");
        assert_eq!(cell(Some(&json!({"k": 1}))), r#"{"k":1}"#);
    }

    #[test]
    fn related_report_columns_read_the_flattened_key() {
        // A List report asked for account.industry; the row carries it split in two.
        let row = json!({"account_industryId": "i1", "account_industryName": "Banking"})
            .as_object()
            .unwrap()
            .clone();
        assert_eq!(cell(row_value(&row, "account.industry")), "Banking");
        // Empty link: Espo keeps the id key at null and drops the name.
        let empty = json!({"account_industryId": null}).as_object().unwrap().clone();
        assert_eq!(cell(row_value(&empty, "account.industry")), "");
        // A scalar through a link, and a link column of the entity itself.
        let mixed = json!({"account_type": "Customer", "campaignName": "Spring 2026"})
            .as_object()
            .unwrap()
            .clone();
        assert_eq!(cell(row_value(&mixed, "account.type")), "Customer");
        assert_eq!(cell(row_value(&mixed, "campaign")), "Spring 2026");
        // An own column still wins over any flattened lookalike.
        let own = json!({"name": "R1", "nameName": "wrong"}).as_object().unwrap().clone();
        assert_eq!(cell(row_value(&own, "name")), "R1");
        assert_eq!(cell(row_value(&own, "missing")), "");
    }

    #[test]
    fn columns_fall_back_to_first_row_keys() {
        let rows = vec![json!({"b": 1, "a": 2})];
        assert_eq!(columns(&rows, &[]), vec!["a", "b"]);
        assert_eq!(columns(&rows, &["z".to_string()]), vec!["z"]);
        assert!(columns(&[], &[]).is_empty());
    }
}

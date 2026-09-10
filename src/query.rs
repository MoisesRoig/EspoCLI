//! Filter mini-DSL to Espo searchParams, and metadata-driven coercion of `field=value` pairs.

use crate::client::usage;
use crate::meta::Meta;
use anyhow::Result;
use serde_json::{Map, Value, json};

/// Longest tokens first: `!=` must win over `=` at the same position.
const OPS: &[(&str, &str)] = &[
    ("!:", "notIn"),
    ("!=", "notEquals"),
    (">=", "greaterThanOrEquals"),
    ("<=", "lessThanOrEquals"),
    (":", "in"),
    ("=", "equals"),
    (">", "greaterThan"),
    ("<", "lessThan"),
    ("~", "contains"),
    ("^", "startsWith"),
];

fn split_op(expr: &str) -> Option<(&str, &'static str, &str)> {
    for i in 0..expr.len() {
        if !expr.is_char_boundary(i) {
            continue;
        }
        for (token, kind) in OPS {
            if expr[i..].starts_with(token) {
                return Some((&expr[..i], kind, &expr[i + token.len()..]));
            }
        }
    }
    None
}

/// Field names referenced by the filters, so list can select them without being asked.
pub fn where_fields(exprs: &[String]) -> Vec<String> {
    exprs
        .iter()
        .filter_map(|e| split_op(e).map(|(field, _, _)| field.trim().to_string()))
        .filter(|f| !f.is_empty())
        .collect()
}

pub fn parse_where(exprs: &[String], meta: &Meta, entity: &str) -> Result<Vec<Value>> {
    exprs.iter().map(|e| parse_one(e, meta, entity)).collect()
}

fn parse_one(expr: &str, meta: &Meta, entity: &str) -> Result<Value> {
    let Some((field, kind, raw)) = split_op(expr) else {
        return Err(usage(format!(
            "cannot parse filter {expr:?}; expected field=value, field!=value, field~text, field:a,b, field>value"
        )));
    };
    let field = field.trim();
    if field.is_empty() {
        return Err(usage(format!("filter {expr:?} has no field name")));
    }
    match (kind, raw) {
        ("equals", "null") => Ok(json!({"type": "isNull", "attribute": field})),
        ("notEquals", "null") => Ok(json!({"type": "isNotNull", "attribute": field})),
        ("in" | "notIn", _) => {
            let values: Vec<Value> =
                raw.split(',').map(|p| coerce(meta, entity, field, p.trim())).collect();
            if values.is_empty() {
                return Err(usage(format!("filter {expr:?} has an empty value list")));
            }
            Ok(json!({"type": kind, "attribute": field, "value": values}))
        }
        _ => Ok(json!({"type": kind, "attribute": field, "value": coerce(meta, entity, field, raw)})),
    }
}

/// Converts a raw string to the field's declared type; unknown fields stay strings.
pub fn coerce(meta: &Meta, entity: &str, field: &str, raw: &str) -> Value {
    let kind = meta.field_type(entity, field).or_else(|| {
        // `teamsIds` is not a field; its link `teams` is, and a linkMultiple takes a list.
        field
            .strip_suffix("Ids")
            .and_then(|base| meta.field_type(entity, base))
            .filter(|k| *k == "linkMultiple")
    });
    match kind {
        Some("bool") => match raw {
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            _ => Value::String(raw.to_string()),
        },
        Some("int") => raw.parse::<i64>().map(Value::from).unwrap_or_else(|_| raw.into()),
        Some("float" | "currency" | "currencyConverted") => {
            raw.parse::<f64>().map(Value::from).unwrap_or_else(|_| raw.into())
        }
        Some("array" | "multiEnum" | "checklist" | "linkMultiple" | "urlMultiple") => {
            if raw.is_empty() {
                Value::Array(Vec::new())
            } else {
                Value::Array(raw.split(',').map(|p| Value::String(p.trim().to_string())).collect())
            }
        }
        _ => Value::String(raw.to_string()),
    }
}

/// Parses `field=value` and `field:=<json>` arguments into a request body.
pub fn parse_assignments(args: &[String], meta: &Meta, entity: &str) -> Result<Map<String, Value>> {
    let mut out = Map::new();
    for arg in args {
        let json_at = arg.find(":=");
        let eq_at = arg.find('=');
        match (json_at, eq_at) {
            (Some(i), Some(j)) if i <= j => {
                let value = serde_json::from_str(&arg[i + 2..])
                    .map_err(|e| usage(format!("invalid JSON in {arg:?}: {e}")))?;
                out.insert(field_name(&arg[..i], arg)?, value);
            }
            (_, Some(j)) => {
                let field = field_name(&arg[..j], arg)?;
                let value = coerce(meta, entity, &field, &arg[j + 1..]);
                out.insert(field, value);
            }
            _ => return Err(usage(format!("expected field=value or field:=<json>, got {arg:?}"))),
        }
    }
    Ok(out)
}

fn field_name(candidate: &str, arg: &str) -> Result<String> {
    let name = candidate.trim();
    if name.is_empty() {
        return Err(usage(format!("argument {arg:?} has no field name")));
    }
    Ok(name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta() -> Meta {
        Meta::from_value(json!({
            "entityDefs": {
                "Lead": {
                    "fields": {
                        "name": {"type": "personName"},
                        "status": {"type": "enum", "options": ["New", "Dead"]},
                        "isActive": {"type": "bool"},
                        "amount": {"type": "int"},
                        "rate": {"type": "float"},
                        "tags": {"type": "array"},
                        "teams": {"type": "linkMultiple"},
                        "phoneNumber": {"type": "phone"}
                    },
                    "links": {"teams": {"type": "hasMany", "entity": "Team"}}
                }
            }
        }))
    }

    fn one(expr: &str) -> Value {
        parse_one(expr, &meta(), "Lead").unwrap()
    }

    #[test]
    fn operators_map_to_espo_types() {
        assert_eq!(one("status=New"), json!({"type":"equals","attribute":"status","value":"New"}));
        assert_eq!(one("status!=New"), json!({"type":"notEquals","attribute":"status","value":"New"}));
        assert_eq!(one("name~puig"), json!({"type":"contains","attribute":"name","value":"puig"}));
        assert_eq!(one("name^pu"), json!({"type":"startsWith","attribute":"name","value":"pu"}));
        assert_eq!(one("amount>=5"), json!({"type":"greaterThanOrEquals","attribute":"amount","value":5}));
        assert_eq!(one("amount>5"), json!({"type":"greaterThan","attribute":"amount","value":5}));
        assert_eq!(one("amount<=5"), json!({"type":"lessThanOrEquals","attribute":"amount","value":5}));
        assert_eq!(one("amount<5"), json!({"type":"lessThan","attribute":"amount","value":5}));
    }

    #[test]
    fn null_and_list_forms() {
        assert_eq!(one("assignedUserId=null"), json!({"type":"isNull","attribute":"assignedUserId"}));
        assert_eq!(one("assignedUserId!=null"), json!({"type":"isNotNull","attribute":"assignedUserId"}));
        assert_eq!(one("status:New,Dead"), json!({"type":"in","attribute":"status","value":["New","Dead"]}));
        assert_eq!(one("status!:Dead"), json!({"type":"notIn","attribute":"status","value":["Dead"]}));
    }

    #[test]
    fn coercion_follows_declared_types() {
        let m = meta();
        assert_eq!(coerce(&m, "Lead", "isActive", "true"), json!(true));
        assert_eq!(coerce(&m, "Lead", "amount", "42"), json!(42));
        assert_eq!(coerce(&m, "Lead", "rate", "1.5"), json!(1.5));
        assert_eq!(coerce(&m, "Lead", "tags", "a, b"), json!(["a", "b"]));
        assert_eq!(coerce(&m, "Lead", "teamsIds", "t1,t2"), json!(["t1", "t2"]));
        // A leading-zero phone must not become a number, and unknown fields stay strings.
        assert_eq!(coerce(&m, "Lead", "phoneNumber", "0034931234567"), json!("0034931234567"));
        assert_eq!(coerce(&m, "Lead", "whatever", "12"), json!("12"));
        assert_eq!(coerce(&m, "Lead", "amount", "not-a-number"), json!("not-a-number"));
    }

    #[test]
    fn assignments_accept_raw_json_and_reject_garbage() {
        let m = meta();
        let body = parse_assignments(
            &["status=New".into(), "isActive=true".into(), "tags:=[\"x\"]".into()],
            &m,
            "Lead",
        )
        .unwrap();
        assert_eq!(body["status"], json!("New"));
        assert_eq!(body["isActive"], json!(true));
        assert_eq!(body["tags"], json!(["x"]));
        assert!(parse_assignments(&["nope".into()], &m, "Lead").is_err());
        assert!(parse_assignments(&["=x".into()], &m, "Lead").is_err());
        assert!(parse_assignments(&["a:=notjson".into()], &m, "Lead").is_err());
    }

    #[test]
    fn unparseable_filters_error() {
        assert!(parse_one("statusNew", &meta(), "Lead").is_err());
        assert!(parse_one("=New", &meta(), "Lead").is_err());
    }

    #[test]
    fn where_fields_are_extracted_for_default_select() {
        let exprs = vec!["status=New".to_string(), "amount>=5".to_string(), "junk".to_string()];
        assert_eq!(where_fields(&exprs), vec!["status", "amount"]);
    }
}

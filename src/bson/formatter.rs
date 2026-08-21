//! BSON value formatting utilities for display and editing.

use base64::Engine as _;
use mongodb::bson::Bson;

/// Get a human-readable type label for a BSON value.
pub fn bson_type_label(value: &Bson) -> &'static str {
    match value {
        Bson::Document(_) => "Document",
        Bson::Array(_) => "Array",
        Bson::String(_) => "String",
        Bson::Int32(_) => "Int32",
        Bson::Int64(_) => "Int64",
        Bson::Double(_) => "Double",
        Bson::Boolean(_) => "Bool",
        Bson::Null => "Null",
        Bson::ObjectId(_) => "ObjectId",
        Bson::DateTime(_) => "Date",
        Bson::Binary(_) => "Binary",
        Bson::Decimal128(_) => "Decimal128",
        Bson::Timestamp(_) => "Timestamp",
        Bson::RegularExpression(_) => "Regex",
        Bson::JavaScriptCode(_) => "JavaScript",
        Bson::JavaScriptCodeWithScope(_) => "JavaScriptWithScope",
        Bson::Symbol(_) => "Symbol",
        Bson::Undefined => "Undefined",
        Bson::MinKey => "MinKey",
        Bson::MaxKey => "MaxKey",
        Bson::DbPointer(_) => "DBPointer",
    }
}

/// Get a preview string for a BSON value, truncated to max_len.
pub fn bson_value_preview(value: &Bson, max_len: usize) -> String {
    match value {
        Bson::String(s) => {
            let sanitized = sanitize_for_preview(s);
            truncate_for_preview(&sanitized, max_len)
        }
        Bson::Int32(n) => n.to_string(),
        Bson::Int64(n) => n.to_string(),
        Bson::Double(n) => n.to_string(),
        Bson::Boolean(b) => b.to_string(),
        Bson::Null => "null".to_string(),
        Bson::ObjectId(oid) => oid.to_hex(),
        Bson::DateTime(dt) => (*dt).try_to_rfc3339_string().unwrap_or_else(|_| format!("{dt:?}")),
        Bson::Document(doc) => format!("{{{} fields}}", doc.len()),
        Bson::Array(arr) => format!("[{} items]", arr.len()),
        Bson::Decimal128(value) => value.to_string(),
        Bson::Timestamp(value) => format!("Timestamp({}, {})", value.time, value.increment),
        Bson::Binary(value) => {
            let encoded = base64::engine::general_purpose::STANDARD.encode(&value.bytes);
            truncate_for_preview(
                &format!("BinData({:02x}, \"{encoded}\")", u8::from(value.subtype)),
                max_len,
            )
        }
        Bson::RegularExpression(value) => truncate_for_preview(
            &format!("/{}/{}", escape_regex_pattern(&value.pattern), value.options),
            max_len,
        ),
        Bson::JavaScriptCode(code) => {
            truncate_for_preview(&format!("Code(\"{}\")", sanitize_for_preview(code)), max_len)
        }
        Bson::JavaScriptCodeWithScope(code) => truncate_for_preview(
            &format!(
                "CodeWithScope(\"{}\", {{{} fields}})",
                sanitize_for_preview(&code.code),
                code.scope.len()
            ),
            max_len,
        ),
        Bson::Symbol(value) => truncate_for_preview(value, max_len),
        Bson::Undefined => "undefined".to_string(),
        Bson::MinKey => "MinKey()".to_string(),
        Bson::MaxKey => "MaxKey()".to_string(),
        Bson::DbPointer(value) => truncate_for_preview(&format!("{value:?}"), max_len),
    }
}

/// Get a BSON value formatted for editing in an input field.
pub fn bson_value_for_edit(value: &Bson) -> String {
    match value {
        Bson::String(s) => s.clone(),
        Bson::Int32(n) => n.to_string(),
        Bson::Int64(n) => n.to_string(),
        Bson::Double(n) => n.to_string(),
        Bson::Boolean(b) => b.to_string(),
        Bson::Null => "null".to_string(),
        Bson::ObjectId(oid) => oid.to_hex(),
        Bson::DateTime(dt) => (*dt).try_to_rfc3339_string().unwrap_or_else(|_| format!("{dt:?}")),
        Bson::Decimal128(value) => format!("NumberDecimal(\"{value}\")"),
        Bson::Timestamp(value) => format!("Timestamp({}, {})", value.time, value.increment),
        Bson::RegularExpression(_) | Bson::Binary(_) | Bson::DbPointer(_) => {
            serde_json::to_string(&value.clone().into_relaxed_extjson())
                .unwrap_or_else(|_| format!("{value:?}"))
        }
        Bson::Symbol(value) | Bson::JavaScriptCode(value) => value.clone(),
        Bson::Undefined => "undefined".to_string(),
        Bson::MinKey => "MinKey()".to_string(),
        Bson::MaxKey => "MaxKey()".to_string(),
        other => format!("{other:?}"),
    }
}

/// Truncate a string for preview display, adding ellipsis if needed.
pub fn truncate_for_preview(input: &str, max_len: usize) -> String {
    if input.chars().count() <= max_len {
        return input.to_string();
    }

    let mut output = String::new();
    for (idx, ch) in input.chars().enumerate() {
        if idx >= max_len.saturating_sub(3) {
            break;
        }
        output.push(ch);
    }
    output.push_str("...");
    output
}

fn sanitize_for_preview(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            ch if ch.is_control() => output.push('?'),
            _ => output.push(ch),
        }
    }
    output
}

fn escape_regex_pattern(input: &str) -> String {
    input.replace('\\', "\\\\").replace('/', "\\/")
}

#[cfg(test)]
mod tests {
    use mongodb::bson::{Binary, Decimal128, Regex, Timestamp, spec::BinarySubtype};

    use super::*;

    #[test]
    fn labels_cover_common_bson_types() {
        let cases = [
            (Bson::Timestamp(Timestamp { time: 5, increment: 7 }), "Timestamp"),
            (Bson::RegularExpression(Regex { pattern: "^a".into(), options: "i".into() }), "Regex"),
            (Bson::Symbol("symbol".into()), "Symbol"),
            (Bson::Undefined, "Undefined"),
            (Bson::MinKey, "MinKey"),
            (Bson::MaxKey, "MaxKey"),
        ];
        for (value, expected) in cases {
            assert_eq!(bson_type_label(&value), expected);
        }
    }

    #[test]
    fn previews_use_shell_friendly_bson_syntax() {
        let decimal = "12.50".parse::<Decimal128>().expect("decimal");
        assert_eq!(bson_value_preview(&Bson::Decimal128(decimal), 80), "12.50");
        assert_eq!(
            bson_value_preview(&Bson::Timestamp(Timestamp { time: 5, increment: 7 }), 80),
            "Timestamp(5, 7)"
        );
        assert_eq!(
            bson_value_preview(
                &Bson::RegularExpression(Regex { pattern: "a/b".into(), options: "i".into() }),
                80,
            ),
            "/a\\/b/i"
        );
        assert_eq!(
            bson_value_preview(
                &Bson::Binary(Binary { subtype: BinarySubtype::Generic, bytes: vec![1, 2, 3] }),
                80,
            ),
            "BinData(00, \"AQID\")"
        );
    }
}

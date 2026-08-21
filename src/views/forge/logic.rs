use mongodb::bson::{Bson, Document};
use regex::Regex;
use std::sync::LazyLock;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggestion {
    pub label: String,
    pub kind: SuggestionKind,
    pub insert_text: String,
    pub is_snippet: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SuggestionKind {
    Collection,
    Method,
    Operator,
    Field,
}

impl SuggestionKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            SuggestionKind::Collection => "Collection",
            SuggestionKind::Method => "Method",
            SuggestionKind::Operator => "Operator",
            SuggestionKind::Field => "Field",
        }
    }
}

pub fn statement_bounds(text: &str, cursor: usize) -> (usize, usize) {
    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut start = cursor.min(len);
    let mut end = cursor.min(len);

    let mut i = start;
    while i > 0 {
        let b = bytes[i - 1];
        if b == b';' {
            start = i;
            break;
        }
        if i >= 2 && bytes[i - 2] == b'\n' && b == b'\n' {
            start = i;
            break;
        }
        i -= 1;
        start = i;
    }

    let mut j = end;
    while j < len {
        let b = bytes[j];
        if b == b';' {
            end = j + 1;
            break;
        }
        if j + 1 < len && b == b'\n' && bytes[j + 1] == b'\n' {
            end = j + 1;
            break;
        }
        j += 1;
        end = j;
    }

    (start, end)
}

pub fn db_method_template(name: &str) -> Option<&'static str> {
    match name {
        "stats" => Some("stats()"),
        "getCollection" => Some("getCollection(\"$1\")$0"),
        "getSiblingDB" => Some("getSiblingDB(\"$1\")$0"),
        "runCommand" => Some("runCommand({$1})$0"),
        "listCollections" => Some("listCollections({$1})$0"),
        "createCollection" => Some("createCollection(\"$1\")$0"),
        _ => None,
    }
}

pub fn collection_method_template(name: &str) -> Option<&'static str> {
    match name {
        "find" => Some("find({$1})$0"),
        "findOne" => Some("findOne({$1})$0"),
        "aggregate" => Some("aggregate([{$1}])$0"),
        "insertOne" => Some("insertOne({$1})$0"),
        "insertMany" => Some("insertMany([{$1}])$0"),
        "updateOne" => Some("updateOne({$1}, {$2})$0"),
        "updateMany" => Some("updateMany({$1}, {$2})$0"),
        "deleteOne" => Some("deleteOne({$1})$0"),
        "deleteMany" => Some("deleteMany({$1})$0"),
        "countDocuments" => Some("countDocuments({$1})$0"),
        "distinct" => Some("distinct(\"$1\")$0"),
        "createIndex" => Some("createIndex({$1})$0"),
        "dropIndex" => Some("dropIndex(\"$1\")$0"),
        "getIndexes" => Some("getIndexes()"),
        _ => None,
    }
}

pub fn label_from_template(template: &str) -> String {
    let mut out = String::new();
    let chars: Vec<char> = template.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        if ch == '$' {
            if i + 1 < chars.len() && chars[i + 1] == '{' {
                let mut j = i + 2;
                while j < chars.len() && chars[j].is_ascii_digit() {
                    j += 1;
                }
                if j < chars.len() && chars[j] == ':' {
                    j += 1;
                    let default_start = j;
                    while j < chars.len() && chars[j] != '}' {
                        j += 1;
                    }
                    out.extend(chars[default_start..j].iter());
                    if j < chars.len() && chars[j] == '}' {
                        j += 1;
                    }
                    i = j;
                    continue;
                }
                if j < chars.len() && chars[j] == '}' {
                    i = j + 1;
                    continue;
                }
            }
            if i + 1 < chars.len() && chars[i + 1].is_ascii_digit() {
                let mut j = i + 1;
                while j < chars.len() && chars[j].is_ascii_digit() {
                    j += 1;
                }
                i = j;
                continue;
            }
        }
        out.push(ch);
        i += 1;
    }
    out
}

pub const METHODS: &[&str] = &[
    "find",
    "findOne",
    "aggregate",
    "insertOne",
    "insertMany",
    "updateOne",
    "updateMany",
    "deleteOne",
    "deleteMany",
    "countDocuments",
    "distinct",
    "createIndex",
    "dropIndex",
    "getIndexes",
];

pub const PIPELINE_OPERATORS: &[&str] = &[
    "$match",
    "$project",
    "$group",
    "$sort",
    "$limit",
    "$skip",
    "$unwind",
    "$lookup",
    "$addFields",
    "$set",
    "$unset",
    "$replaceRoot",
    "$replaceWith",
    "$bucket",
    "$bucketAuto",
    "$count",
    "$facet",
    "$out",
    "$merge",
    "$sample",
    "$unionWith",
    "$redact",
    "$graphLookup",
];

pub const QUERY_OPERATORS: &[&str] = &[
    "$eq",
    "$ne",
    "$gt",
    "$gte",
    "$lt",
    "$lte",
    "$in",
    "$nin",
    "$exists",
    "$regex",
    "$and",
    "$or",
    "$nor",
    "$not",
    "$elemMatch",
    "$size",
    "$all",
    "$type",
];

pub const UPDATE_OPERATORS: &[&str] = &[
    "$set",
    "$unset",
    "$inc",
    "$push",
    "$addToSet",
    "$pull",
    "$pop",
    "$rename",
    "$mul",
    "$min",
    "$max",
    "$currentDate",
];

pub fn format_printable_lines(printable: &serde_json::Value) -> Vec<String> {
    if printable.is_null() {
        return vec!["null".to_string()];
    }
    if let Some(text) = printable.as_str() {
        if text.is_empty() {
            return Vec::new();
        }
        return text.split('\n').map(|line| line.to_string()).collect();
    }
    let text = serde_json::to_string_pretty(printable).unwrap_or_else(|_| printable.to_string());
    text.split('\n').map(|line| line.to_string()).collect()
}

pub fn is_trivial_printable(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => true,
        serde_json::Value::String(text) => {
            let trimmed = text.trim();
            trimmed.is_empty() || trimmed.eq_ignore_ascii_case("undefined")
        }
        _ => false,
    }
}

pub fn default_result_label_for_value(value: &serde_json::Value) -> String {
    if value.is_array() {
        "Shell Output (Array)".to_string()
    } else {
        "Shell Output (Documents)".to_string()
    }
}

pub fn result_documents(printable: &serde_json::Value) -> Option<Vec<Document>> {
    if let Some(text) = printable.as_str() {
        let trimmed = text.trim();
        if (trimmed.starts_with('{') || trimmed.starts_with('['))
            && let Ok(value) = crate::bson::parse_value_from_relaxed_json(trimmed)
        {
            return documents_from_json_value(&value);
        }
        return None;
    }

    if let Some(docs) = cursor_documents(printable) {
        return Some(docs);
    }

    documents_from_json_value(printable)
}

fn documents_from_json_value(value: &serde_json::Value) -> Option<Vec<Document>> {
    match value {
        serde_json::Value::Object(_) => {
            let bson = decode_extjson_node(value);
            if let Bson::Document(doc) = bson {
                return Some(vec![doc]);
            }
            None
        }
        serde_json::Value::Array(items) => {
            let mut docs = Vec::with_capacity(items.len());
            for item in items {
                let bson = decode_extjson_node(item);
                if let Bson::Document(doc) = bson {
                    docs.push(doc);
                } else {
                    return None;
                }
            }
            if docs.is_empty() { None } else { Some(docs) }
        }
        _ => None,
    }
}

fn cursor_documents(printable: &serde_json::Value) -> Option<Vec<Document>> {
    let obj = printable.as_object()?;
    let docs = obj.get("documents")?.as_array()?;
    if docs.is_empty() {
        return None;
    }

    let mut out = Vec::with_capacity(docs.len());
    for item in docs {
        let bson = decode_extjson_node(item);
        match bson {
            Bson::Document(doc) => out.push(doc),
            other => {
                let mut doc = Document::new();
                doc.insert("value", other);
                out.push(doc);
            }
        }
    }

    if out.is_empty() { None } else { Some(out) }
}

fn decode_extjson_node(value: &serde_json::Value) -> Bson {
    match value {
        serde_json::Value::Null => Bson::Null,
        serde_json::Value::Bool(val) => Bson::Boolean(*val),
        serde_json::Value::Number(num) => {
            if let Some(val) = num.as_i64() {
                Bson::Int64(val)
            } else if let Some(val) = num.as_u64() {
                if val <= i64::MAX as u64 {
                    Bson::Int64(val as i64)
                } else if let Some(val) = num.as_f64() {
                    Bson::Double(val)
                } else {
                    Bson::String(num.to_string())
                }
            } else if let Some(val) = num.as_f64() {
                Bson::Double(val)
            } else {
                Bson::String(num.to_string())
            }
        }
        serde_json::Value::String(val) => Bson::String(val.clone()),
        serde_json::Value::Array(items) => {
            Bson::Array(items.iter().map(decode_extjson_node).collect())
        }
        serde_json::Value::Object(map) => {
            if is_extended_json_wrapper(map)
                && let Ok(decoded) = Bson::try_from(value.clone())
            {
                return decoded;
            }
            let mut doc = Document::new();
            for (key, val) in map {
                doc.insert(key, decode_extjson_node(val));
            }
            Bson::Document(doc)
        }
    }
}

fn is_extended_json_wrapper(map: &serde_json::Map<String, serde_json::Value>) -> bool {
    if map.len() == 2 && map.contains_key("$code") && map.contains_key("$scope") {
        return true;
    }
    if map.len() != 1 {
        return false;
    }
    matches!(
        map.keys().next().map(String::as_str),
        Some(
            "$oid"
                | "$date"
                | "$numberInt"
                | "$numberLong"
                | "$numberDouble"
                | "$numberDecimal"
                | "$binary"
                | "$timestamp"
                | "$regularExpression"
                | "$code"
                | "$undefined"
                | "$minKey"
                | "$maxKey"
                | "$symbol"
                | "$dbPointer"
        )
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExactFindMethod {
    Find,
    FindOne,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExactFindOrigin {
    pub collection: String,
    pub method: ExactFindMethod,
}

static EXACT_FIND_PREFIX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?s)^\s*db\s*(?:\.\s*getCollection\s*\(\s*(?:\"([^\"\\]*)\"|'([^'\\]*)')\s*\)|\.\s*([A-Za-z_][A-Za-z0-9_$]*))\s*\.\s*(findOne|find)\s*\("#,
    )
    .expect("exact Forge find regex")
});

/// Resolve provenance only for one unambiguous `find`/`findOne` expression.
/// Any second database reference, extra statement, or non-call chain is rejected.
pub fn exact_find_origin(code: &str) -> Option<ExactFindOrigin> {
    if count_db_references(code) != 1 || code.contains("getSiblingDB") {
        return None;
    }
    let expression_start = skip_js_trivia(code, 0)?;
    let captures = EXACT_FIND_PREFIX.captures(&code[expression_start..])?;
    let whole = captures.get(0)?;
    let collection = captures
        .get(1)
        .or_else(|| captures.get(2))
        .or_else(|| captures.get(3))?
        .as_str()
        .to_string();
    let method = match captures.get(4)?.as_str() {
        "find" => ExactFindMethod::Find,
        "findOne" => ExactFindMethod::FindOne,
        _ => return None,
    };

    let call_open = expression_start + whole.end().saturating_sub(1);
    let call_close = matching_call_close(code, call_open)?;
    let mut cursor = call_close + 1;
    loop {
        cursor = skip_js_trivia(code, cursor)?;
        if cursor >= code.len() {
            break;
        }
        if code.as_bytes()[cursor] == b';' {
            cursor = skip_js_trivia(code, cursor + 1)?;
            if cursor != code.len() {
                return None;
            }
            break;
        }
        if code.as_bytes()[cursor] != b'.' {
            return None;
        }
        cursor = skip_js_trivia(code, cursor + 1)?;
        let method_start = cursor;
        while cursor < code.len()
            && (code.as_bytes()[cursor].is_ascii_alphanumeric()
                || matches!(code.as_bytes()[cursor], b'_' | b'$'))
        {
            cursor += 1;
        }
        if cursor == method_start {
            return None;
        }
        let chained_method = &code[method_start..cursor];
        if !matches!(
            chained_method,
            "sort"
                | "limit"
                | "skip"
                | "hint"
                | "batchSize"
                | "maxTimeMS"
                | "collation"
                | "comment"
                | "allowDiskUse"
                | "toArray"
        ) {
            return None;
        }
        cursor = skip_js_trivia(code, cursor)?;
        if code.as_bytes().get(cursor) != Some(&b'(') {
            return None;
        }
        cursor = matching_call_close(code, cursor)? + 1;
    }

    Some(ExactFindOrigin { collection, method })
}

fn count_db_references(code: &str) -> usize {
    let bytes = code.as_bytes();
    let mut count = 0;
    let mut index = 0;
    let mut quote = None;
    let mut escaped = false;
    let mut line_comment = false;
    let mut block_comment = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if line_comment {
            if byte == b'\n' {
                line_comment = false;
            }
            index += 1;
            continue;
        }
        if block_comment {
            if byte == b'*' && bytes.get(index + 1) == Some(&b'/') {
                block_comment = false;
                index += 2;
            } else {
                index += 1;
            }
            continue;
        }
        if let Some(active_quote) = quote {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == active_quote {
                quote = None;
            }
            index += 1;
            continue;
        }
        if byte == b'/' && bytes.get(index + 1) == Some(&b'/') {
            line_comment = true;
            index += 2;
            continue;
        }
        if byte == b'/' && bytes.get(index + 1) == Some(&b'*') {
            block_comment = true;
            index += 2;
            continue;
        }
        if matches!(byte, b'\'' | b'"') {
            quote = Some(byte);
            index += 1;
            continue;
        }
        let is_token_start = index == 0 || !is_js_identifier_byte(bytes[index - 1]);
        if is_token_start && byte == b'd' && bytes.get(index + 1) == Some(&b'b') {
            let after_db = index + 2;
            if bytes.get(after_db).is_none_or(|next| !is_js_identifier_byte(*next)) {
                let after_space = skip_ascii_whitespace(code, after_db);
                if bytes.get(after_space) == Some(&b'.') {
                    count += 1;
                }
            }
        }
        index += 1;
    }
    count
}

fn is_js_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$')
}

fn skip_ascii_whitespace(code: &str, mut cursor: usize) -> usize {
    while code.as_bytes().get(cursor).is_some_and(u8::is_ascii_whitespace) {
        cursor += 1;
    }
    cursor
}

fn skip_js_trivia(code: &str, mut cursor: usize) -> Option<usize> {
    let bytes = code.as_bytes();
    loop {
        cursor = skip_ascii_whitespace(code, cursor);
        if bytes.get(cursor) == Some(&b'/') && bytes.get(cursor + 1) == Some(&b'/') {
            cursor += 2;
            while cursor < bytes.len() && bytes[cursor] != b'\n' {
                cursor += 1;
            }
            continue;
        }
        if bytes.get(cursor) == Some(&b'/') && bytes.get(cursor + 1) == Some(&b'*') {
            cursor += 2;
            loop {
                if cursor + 1 >= bytes.len() {
                    return None;
                }
                if bytes[cursor] == b'*' && bytes[cursor + 1] == b'/' {
                    cursor += 2;
                    break;
                }
                cursor += 1;
            }
            continue;
        }
        return Some(cursor);
    }
}

fn matching_call_close(code: &str, open: usize) -> Option<usize> {
    if code.as_bytes().get(open) != Some(&b'(') {
        return None;
    }
    let bytes = code.as_bytes();
    let mut stack = vec![b')'];
    let mut quote = None;
    let mut escaped = false;
    let mut line_comment = false;
    let mut block_comment = false;
    let mut index = open + 1;
    while index < bytes.len() {
        let byte = bytes[index];
        if line_comment {
            if byte == b'\n' {
                line_comment = false;
            }
            index += 1;
            continue;
        }
        if block_comment {
            if byte == b'*' && bytes.get(index + 1) == Some(&b'/') {
                block_comment = false;
                index += 2;
            } else {
                index += 1;
            }
            continue;
        }
        if let Some(active_quote) = quote {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == active_quote {
                quote = None;
            }
            index += 1;
            continue;
        }
        if byte == b'/' && bytes.get(index + 1) == Some(&b'/') {
            line_comment = true;
            index += 2;
            continue;
        }
        if byte == b'/' && bytes.get(index + 1) == Some(&b'*') {
            block_comment = true;
            index += 2;
            continue;
        }
        match byte {
            b'\'' | b'"' => quote = Some(byte),
            b'(' => stack.push(b')'),
            b'[' => stack.push(b']'),
            b'{' => stack.push(b'}'),
            b')' | b']' | b'}' => {
                if stack.pop() != Some(byte) {
                    return None;
                }
                if stack.is_empty() {
                    return Some(index);
                }
            }
            b'`' => return None,
            _ => {}
        }
        index += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use mongodb::bson::oid::ObjectId;

    #[test]
    fn statement_bounds_respects_semicolons() {
        let text = "db.stats();\n\ndb.getCollection(\"x\")";
        let (start, end) = statement_bounds(text, 5);
        assert_eq!(&text[start..end], "db.stats();");
    }

    #[test]
    fn statement_bounds_falls_back_to_paragraph() {
        let text = "db.stats()\n\n// comment\n";
        let (start, end) = statement_bounds(text, 2);
        assert_eq!(&text[start..end], "db.stats()\n");
    }

    #[test]
    fn cursor_documents_decode_extended_json_bson_scalars() {
        let printable = serde_json::json!({
            "documents": [{
                "_id": { "$oid": "6283a37e34d71078c4996c72" },
                "createdAt": { "$date": "2026-08-21T12:34:56Z" }
            }]
        });

        let documents = result_documents(&printable).expect("cursor documents");
        let expected_id = ObjectId::parse_str("6283a37e34d71078c4996c72").expect("valid object id");

        assert_eq!(documents[0].get_object_id("_id").expect("decoded ObjectId"), expected_id);
        assert!(documents[0].get_datetime("createdAt").is_ok());
    }

    #[test]
    fn malformed_extended_json_wrapper_only_degrades_that_node() {
        let printable = serde_json::json!({
            "documents": [{
                "_id": { "$oid": "6283a37e34d71078c4996c72" },
                "bad": { "$oid": "not-an-object-id" },
                "nested": { "date": { "$date": "2026-08-21T12:34:56Z" } }
            }]
        });
        let documents = result_documents(&printable).expect("cursor documents");
        assert!(documents[0].get_object_id("_id").is_ok());
        assert!(documents[0].get_document("bad").is_ok());
        assert!(documents[0].get_document("nested").unwrap().get_datetime("date").is_ok());
    }

    #[test]
    fn array_results_decode_extended_json_per_node() {
        let printable = serde_json::json!([{
            "_id": { "$oid": "6283a37e34d71078c4996c72" },
            "amount": { "$numberDecimal": "12.50" }
        }]);
        let documents = result_documents(&printable).expect("array documents");
        assert!(documents[0].get_object_id("_id").is_ok());
        assert!(matches!(documents[0].get("amount"), Some(Bson::Decimal128(_))));
    }

    #[test]
    fn common_extended_json_wrappers_decode_without_changing_sibling_keys() {
        let printable = serde_json::json!({
            "oid": { "$oid": "6283a37e34d71078c4996c72" },
            "date": { "$date": "2026-08-21T12:34:56Z" },
            "dateCanonical": { "$date": { "$numberLong": "1787315696000" } },
            "int": { "$numberInt": "7" },
            "long": { "$numberLong": "9223372036854775806" },
            "double": { "$numberDouble": "7.0" },
            "decimal": { "$numberDecimal": "12.50" },
            "binary": { "$binary": { "base64": "AQID", "subType": "00" } },
            "timestamp": { "$timestamp": { "t": 5, "i": 7 } },
            "regex": { "$regularExpression": { "pattern": "^a", "options": "i" } },
            "code": { "$code": "return 1;" },
            "symbol": { "$symbol": "token" },
            "undefined": { "$undefined": true },
            "min": { "$minKey": 1 },
            "max": { "$maxKey": 1 }
        });
        let documents = result_documents(&printable).expect("document result");
        let document = &documents[0];
        assert_eq!(document.keys().count(), 15);
        assert!(matches!(document.get("oid"), Some(Bson::ObjectId(_))));
        assert!(matches!(document.get("date"), Some(Bson::DateTime(_))));
        assert!(matches!(document.get("dateCanonical"), Some(Bson::DateTime(_))));
        assert!(matches!(document.get("int"), Some(Bson::Int32(_))));
        assert!(matches!(document.get("long"), Some(Bson::Int64(_))));
        assert!(matches!(document.get("double"), Some(Bson::Double(_))));
        assert!(matches!(document.get("decimal"), Some(Bson::Decimal128(_))));
        assert!(matches!(document.get("binary"), Some(Bson::Binary(_))));
        assert!(matches!(document.get("timestamp"), Some(Bson::Timestamp(_))));
        assert!(matches!(document.get("regex"), Some(Bson::RegularExpression(_))));
        assert!(matches!(document.get("code"), Some(Bson::JavaScriptCode(_))));
        assert!(matches!(document.get("symbol"), Some(Bson::Symbol(_))));
        assert!(matches!(document.get("undefined"), Some(Bson::Undefined)));
        assert!(matches!(document.get("min"), Some(Bson::MinKey)));
        assert!(matches!(document.get("max"), Some(Bson::MaxKey)));
    }

    #[test]
    fn string_results_decode_extended_json_per_node() {
        let printable = serde_json::Value::String(
            r#"[{_id: ObjectId("6283a37e34d71078c4996c72"), bad: {$oid: "broken"}}]"#.to_string(),
        );
        let documents = result_documents(&printable).expect("string documents");
        assert!(documents[0].get_object_id("_id").is_ok());
        assert!(documents[0].get_document("bad").is_ok());
    }

    #[test]
    fn exact_origin_accepts_one_find_or_find_one_on_one_collection() {
        assert_eq!(
            exact_find_origin("db.orders.find({status: 'open'}).sort({_id: 1}).limit(20);"),
            Some(ExactFindOrigin { collection: "orders".into(), method: ExactFindMethod::Find })
        );
        assert_eq!(
            exact_find_origin("db.orders.find({}).limit(50).toArray()"),
            Some(ExactFindOrigin { collection: "orders".into(), method: ExactFindMethod::Find })
        );
        assert_eq!(
            exact_find_origin(r#"db.orders.find({note: "db.customers is text"})"#),
            Some(ExactFindOrigin { collection: "orders".into(), method: ExactFindMethod::Find })
        );
        assert_eq!(
            exact_find_origin("db.getCollection(\"order-items\").findOne({_id: id})"),
            Some(ExactFindOrigin {
                collection: "order-items".into(),
                method: ExactFindMethod::FindOne,
            })
        );

        let default_template = concat!(
            "db.getCollection(\"orders\").find({\n",
            "    \n",
            "})\n",
            "// .projection({_id: 1})\n",
            ".sort({createdAt:-1}).limit(10).maxTimeMS(5000)",
        );
        assert_eq!(
            exact_find_origin(default_template),
            Some(ExactFindOrigin { collection: "orders".into(), method: ExactFindMethod::Find })
        );
    }

    #[test]
    fn exact_origin_rejects_ambiguous_or_cross_database_scripts() {
        for code in [
            "db.orders.aggregate([])",
            "db.getSiblingDB('other').orders.find({})",
            "db.orders.find({}); db.customers.find({})",
            "const x = db.orders.find({})",
            "db.orders.find({other: db.customers.findOne({})})",
            "db.orders.find({other: db .customers.findOne({})})",
            "db.orders.find({}).collectionName",
            "db.orders.find({}).map(doc => doc)",
        ] {
            assert_eq!(exact_find_origin(code), None, "{code}");
        }
    }
}

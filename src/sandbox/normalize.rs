use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NormalizedToolValue {
    Null,
    Text(String),
    Json(Value),
    Blocks(Vec<Value>),
}

impl NormalizedToolValue {
    pub(super) fn into_json(self) -> Value {
        match self {
            NormalizedToolValue::Null => Value::Null,
            NormalizedToolValue::Text(text) => Value::String(text),
            NormalizedToolValue::Json(value) => value,
            NormalizedToolValue::Blocks(blocks) => Value::Array(blocks),
        }
    }
}

pub fn normalize_tool_result(value: &Value) -> std::result::Result<NormalizedToolValue, String> {
    let is_error = value
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let structured = value
        .get("structuredContent")
        .filter(|value| !value.is_null());
    if is_error {
        let mut text = content_text(value);
        if text.trim().is_empty() {
            text = "tool returned isError".to_string();
        }
        return Err(text);
    }
    if let Some(structured) = structured {
        return Ok(NormalizedToolValue::Json(structured.clone()));
    }
    let Some(content) = value.get("content").and_then(Value::as_array) else {
        return Ok(NormalizedToolValue::Null);
    };
    if content.is_empty() {
        return Ok(NormalizedToolValue::Null);
    }
    if content.len() == 1 {
        return Ok(normalize_content_block(&content[0]));
    }
    Ok(NormalizedToolValue::Blocks(
        content
            .iter()
            .map(|block| normalize_content_block(block).into_json())
            .collect(),
    ))
}

fn normalize_content_block(block: &Value) -> NormalizedToolValue {
    match block.get("type").and_then(Value::as_str) {
        Some("text") => {
            let text = block
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let trimmed = text.trim();
            if trimmed.starts_with('{') || trimmed.starts_with('[') {
                if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
                    return NormalizedToolValue::Json(value);
                }
            }
            NormalizedToolValue::Text(text.to_string())
        }
        Some("image" | "audio" | "resource") => NormalizedToolValue::Json(block.clone()),
        _ => NormalizedToolValue::Json(block.clone()),
    }
}

fn content_text(value: &Value) -> String {
    value
        .get("content")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// Shared PostToolUse envelope — deserialized once in the router.
///
/// The type parameter `T` is the tool-specific response struct
/// (`ToolResponse`, `GrepToolResponse`, `WebFetchToolResponse`).
#[derive(serde::Deserialize)]
pub struct PostToolUsePayload<T> {
    pub tool_name: Option<String>,
    pub tool_response: T,
}

/// Set `tool_input` on a store payload from a raw JSON string.
///
/// Parses as JSON if valid, otherwise wraps as a string value.
pub fn set_tool_input(payload: &mut serde_json::Value, raw: &str) {
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(raw) {
        payload["tool_input"] = parsed;
    } else {
        payload["tool_input"] = serde_json::Value::String(raw.to_owned());
    }
}

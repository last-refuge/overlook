use base64::{engine::general_purpose::STANDARD, Engine};
use hyper::header::{self, HeaderMap};
use hyper::StatusCode;
use rmcp::{
    model::{ClientCapabilities, ProtocolVersion},
    ErrorData,
};
use serde_json::{json, Value};

use super::super::transport::{
    self, CLIENT_CAPABILITIES_KEY, LEGACY, MODERN, SUPPORTED_VERSIONS, VERSION_KEY,
};
use super::super::write;
use super::{
    auth::{Access, Denial},
    empty, json_response, single_header, Bridge, HttpResponse,
};

impl Bridge {
    pub(super) fn post(&self, headers: HeaderMap, body: &[u8], access: Access) -> HttpResponse {
        if !is_json(&headers) || headers.contains_key(header::CONTENT_ENCODING) {
            return empty(StatusCode::UNSUPPORTED_MEDIA_TYPE);
        }
        if !accepts_mcp(&headers) {
            return empty(StatusCode::NOT_ACCEPTABLE);
        }
        let message: Value = match serde_json::from_slice(body) {
            Ok(message) => message,
            Err(_) => return bad_request(None, ErrorData::parse_error("Invalid JSON", None)),
        };
        let raw_id = message.get("id");
        let id = raw_id.filter(|id| id.is_string() || id.is_i64() || id.is_u64());
        let Some(method) = message.get("method").and_then(Value::as_str).filter(|_| {
            message["jsonrpc"] == "2.0"
                && (raw_id.is_none() || id.is_some())
                && message.get("result").is_none()
                && message.get("error").is_none()
        }) else {
            return bad_request(
                id,
                ErrorData::invalid_request("Expected one JSON-RPC request or notification", None),
            );
        };

        let version = match protocol_version(&headers, &message, method) {
            Ok(version) => version,
            Err(error) => return bad_request(id, error),
        };
        let modern = version == MODERN;
        if id.is_none() {
            return if !modern
                && matches!(
                    method,
                    "notifications/initialized" | "notifications/cancelled"
                )
                && message.get("params").is_none_or(Value::is_object)
            {
                empty(StatusCode::ACCEPTED)
            } else {
                bad_request(
                    None,
                    ErrorData::invalid_request("Unsupported MCP notification", None),
                )
            };
        }
        if modern {
            if let Err(error) = validate_modern_headers(&headers, &message, method) {
                return bad_request(id, error);
            }
            let caps = &message["params"]["_meta"][CLIENT_CAPABILITIES_KEY];
            if !caps.is_object()
                || serde_json::from_value::<ClientCapabilities>(caps.clone()).is_err()
            {
                return bad_request(
                    id,
                    ErrorData::invalid_params("Request _meta requires client capabilities", None),
                );
            }
        }

        if !access.may_write
            && method == "tools/call"
            && message["params"]["name"]
                .as_str()
                .is_some_and(requires_write_scope)
        {
            return self.deny(Denial::WriteScope);
        }

        // Legacy initialization establishes the revision. With optional HTTP sessions
        // disabled, the revision header identifies each subsequent legacy request.
        let legacy_after_initialize = !modern && method != "initialize";
        match transport::handle_http(message, legacy_after_initialize, &*self.call) {
            Some(message) => {
                let status = if modern && message["error"]["code"] == -32601 {
                    StatusCode::NOT_FOUND
                } else {
                    StatusCode::OK
                };
                json_response(status, message)
            }
            None => empty(StatusCode::ACCEPTED),
        }
    }
}

fn protocol_version(
    headers: &HeaderMap,
    message: &Value,
    method: &str,
) -> Result<ProtocolVersion, ErrorData> {
    let header = single_header(headers, "mcp-protocol-version").map_err(|_| mismatch())?;
    let meta_version = message["params"]["_meta"].get(VERSION_KEY);
    if let Some(value) = meta_version {
        let version = value.as_str().ok_or_else(mismatch)?;
        if Some(version) != header || version == LEGACY.as_str() {
            return Err(mismatch());
        }
    }
    match header {
        Some(version) if version == MODERN.as_str() && meta_version.is_some() => Ok(MODERN),
        Some(version) if version == MODERN.as_str() => Err(mismatch()),
        Some(version) if version == LEGACY.as_str() => Ok(LEGACY),
        Some(version) => {
            let requested = serde_json::from_value(Value::String(version.to_owned()))
                .map_err(|_| mismatch())?;
            Err(ErrorData::unsupported_protocol_version(
                requested,
                SUPPORTED_VERSIONS,
            ))
        }
        None if method == "initialize" => Ok(LEGACY),
        None => Err(mismatch()),
    }
}

fn validate_modern_headers(
    headers: &HeaderMap,
    message: &Value,
    method: &str,
) -> Result<(), ErrorData> {
    if single_header(headers, "mcp-method") != Ok(Some(method)) {
        return Err(mismatch());
    }
    let name = single_header(headers, "mcp-name").map_err(|_| mismatch())?;
    let field = match method {
        "tools/call" | "prompts/get" => Some("name"),
        "resources/read" => Some("uri"),
        _ => None,
    };
    match (field, name) {
        (Some(field), Some(name))
            if message["params"][field]
                .as_str()
                .is_some_and(|expected| header_name_matches(name, expected)) =>
        {
            Ok(())
        }
        (None, None) => Ok(()),
        _ => Err(mismatch()),
    }
    // No Overlook tool schema defines x-mcp-header properties.
}

fn header_name_matches(header: &str, expected: &str) -> bool {
    match header
        .strip_prefix("=?base64?")
        .and_then(|value| value.strip_suffix("?="))
    {
        Some(base64) => STANDARD
            .decode(base64)
            .is_ok_and(|decoded| decoded == expected.as_bytes()),
        None => header == expected,
    }
}

fn requires_write_scope(name: &str) -> bool {
    name == "mail_refresh" || write::tools().any(|tool| tool.name == name)
}

fn mismatch() -> ErrorData {
    ErrorData::header_mismatch(
        "Required MCP header missing, invalid, or inconsistent with the body",
        None,
    )
}

fn bad_request(id: Option<&Value>, error: ErrorData) -> HttpResponse {
    let mut message = json!({"jsonrpc": "2.0", "error": error});
    if let Some(id) = id {
        message["id"] = id.clone();
    }
    json_response(StatusCode::BAD_REQUEST, message)
}

fn is_json(headers: &HeaderMap) -> bool {
    let Ok(Some(value)) = single_header(headers, "content-type") else {
        return false;
    };
    let mut parts = value.split(';').map(str::trim);
    matches!(parts.next(), Some(media) if media.eq_ignore_ascii_case("application/json"))
        && parts.all(|param| {
            param.split_once('=').is_some_and(|(key, value)| {
                key.trim().eq_ignore_ascii_case("charset")
                    && value.trim().trim_matches('"').eq_ignore_ascii_case("utf-8")
            })
        })
}

fn accepts_mcp(headers: &HeaderMap) -> bool {
    let mut json = false;
    let mut sse = false;
    for header in headers.get_all(header::ACCEPT) {
        let Ok(value) = header.to_str() else {
            return false;
        };
        for range in value.split(',') {
            let mut parts = range.split(';').map(str::trim);
            let media = parts.next().unwrap_or_default();
            let mut allowed = true;
            for param in parts {
                if let Some((_, value)) = param
                    .split_once('=')
                    .filter(|(key, _)| key.trim().eq_ignore_ascii_case("q"))
                {
                    allowed = value
                        .trim()
                        .parse::<f32>()
                        .is_ok_and(|q| q > 0.0 && q <= 1.0);
                }
            }
            json |= allowed && media.eq_ignore_ascii_case("application/json");
            sse |= allowed && media.eq_ignore_ascii_case("text/event-stream");
        }
    }
    json && sse
}

//! Newline-delimited MCP over stdin and stdout.

use std::io::{self, BufRead, Read, Write};

use rmcp::model::{
    CacheScope, CallToolResult, DiscoverResult, ErrorCode as RpcCode, Implementation,
    InitializeRequestParams, InitializeResult, JsonObject, ListToolsResult, MetaObject,
    ProtocolVersion, RequestMetaObject, ServerCapabilities, Tool, ToolAnnotations,
};
use rmcp::ErrorData;
use serde_json::{json, Value};

use super::contract::{ErrorCode, Failure, Reply, Request};
use super::{read, write};

const MODERN: ProtocolVersion = ProtocolVersion::V_2026_07_28;
const LEGACY: ProtocolVersion = ProtocolVersion::V_2025_11_25;
const SUPPORTED_VERSIONS: &[ProtocolVersion] = &[MODERN, LEGACY];
const VERSION_KEY: &str = "io.modelcontextprotocol/protocolVersion";
const CLIENT_CAPABILITIES_KEY: &str = "io.modelcontextprotocol/clientCapabilities";
const SERVER_INFO_KEY: &str = "io.modelcontextprotocol/serverInfo";
const MAX_FRAME_BYTES: usize = 64 * 1024;

const ACCOUNTS_LIST: &str = "accounts_list";
const ACCOUNT_CAPABILITIES: &str = "account_capabilities";

pub fn serve(call: impl Fn(Request) -> Reply + Send + Sync + 'static) -> anyhow::Result<()> {
    serve_stream(io::stdin().lock(), io::stdout().lock(), call)
}

fn serve_stream(
    mut input: impl BufRead,
    mut output: impl Write,
    call: impl Fn(Request) -> Reply,
) -> anyhow::Result<()> {
    let mut server = Server {
        legacy: LegacyState::New,
        call,
    };
    let mut frame = Vec::new();
    loop {
        frame.clear();
        // take() bounds the allocation even if the peer never writes a newline.
        if input
            .by_ref()
            .take(MAX_FRAME_BYTES as u64 + 1)
            .read_until(b'\n', &mut frame)?
            == 0
        {
            return Ok(());
        }

        let response = if frame.len() > MAX_FRAME_BYTES {
            let error = ErrorData::invalid_request("Request frame exceeds 64 KiB", None);
            write_response(&mut output, &server.error_response(None, error))?;
            if !frame.ends_with(b"\n") {
                input.skip_until(b'\n')?;
            }
            continue;
        } else if !frame.ends_with(b"\n") {
            let error = ErrorData::parse_error("Incomplete request frame", None);
            Some(server.error_response(None, error))
        } else {
            server.handle_frame(&frame)
        };

        if let Some(response) = response {
            write_response(&mut output, &response)?;
        }
    }
}

fn write_response(output: &mut impl Write, response: &Value) -> anyhow::Result<()> {
    serde_json::to_writer(&mut *output, response)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}

#[derive(PartialEq, Eq)]
enum LegacyState {
    New,
    AwaitingInitialized,
    Ready,
}

struct Server<F> {
    legacy: LegacyState,
    call: F,
}

impl<F: Fn(Request) -> Reply> Server<F> {
    fn handle_frame(&mut self, frame: &[u8]) -> Option<Value> {
        let mut message: JsonObject = match serde_json::from_slice::<Value>(frame) {
            Ok(Value::Object(message)) => message,
            Ok(_) => {
                let error = ErrorData::invalid_request("Expected one JSON-RPC message", None);
                return Some(self.error_response(None, error));
            }
            Err(_) => {
                let error = ErrorData::parse_error("Invalid JSON", None);
                return Some(self.error_response(None, error));
            }
        };
        let raw_id = message.remove("id");
        let id = raw_id
            .as_ref()
            .filter(|id| id.is_string() || id.is_i64() || id.is_u64());
        let method = message.remove("method");
        let method = method.as_ref().and_then(Value::as_str);
        let Some(method) = method.filter(|_| {
            message.get("jsonrpc").and_then(Value::as_str) == Some("2.0")
                && (raw_id.is_none() || id.is_some())
                && !message.contains_key("result")
                && !message.contains_key("error")
        }) else {
            let error = ErrorData::invalid_request("Invalid JSON-RPC request", None);
            return Some(self.error_response(id, error));
        };

        let params = match message.remove("params") {
            None => JsonObject::new(),
            Some(Value::Object(params)) => params,
            Some(_) => {
                let error = ErrorData::invalid_params("params must be an object", None);
                return id.map(|id| self.error_response(Some(id), error));
            }
        };
        let Some(id) = id else {
            if method == "notifications/initialized"
                && self.legacy == LegacyState::AwaitingInitialized
            {
                self.legacy = LegacyState::Ready;
            }
            return None;
        };

        Some(match self.dispatch(method, params) {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err(error) => self.error_response(Some(id), error),
        })
    }

    fn error_response(&self, id: Option<&Value>, error: ErrorData) -> Value {
        // Modern MCP permits an omitted id when the request cannot be correlated.
        if id.is_none() && self.legacy == LegacyState::New {
            return json!({"jsonrpc": "2.0", "error": error});
        }
        json!({"jsonrpc": "2.0", "id": id, "error": error})
    }

    fn dispatch(&mut self, method: &str, mut params: JsonObject) -> Result<Value, ErrorData> {
        let meta = match params.remove("_meta") {
            None => RequestMetaObject::new(),
            Some(Value::Object(meta)) => RequestMetaObject::from(meta),
            Some(_) => return Err(ErrorData::invalid_params("_meta must be an object", None)),
        };

        let has_version = meta.contains_key(VERSION_KEY);
        if !has_version {
            match method {
                "initialize" => return self.initialize(params),
                // Legacy clients may ping before completing initialization.
                "ping" => return Ok(json!({})),
                _ => {}
            }
        }

        let modern =
            if has_version || method == "server/discover" || self.legacy == LegacyState::New {
                let Some(version) = meta.protocol_version() else {
                    return Err(ErrorData::invalid_params(
                        "Request _meta requires a protocol version",
                        None,
                    ));
                };
                if !meta
                    .get(CLIENT_CAPABILITIES_KEY)
                    .is_some_and(Value::is_object)
                    || meta.client_capabilities().is_none()
                {
                    return Err(ErrorData::invalid_params(
                        "Request _meta requires client capabilities",
                        None,
                    ));
                }
                if !SUPPORTED_VERSIONS.contains(&version) {
                    return Err(ErrorData::unsupported_protocol_version(
                        version,
                        SUPPORTED_VERSIONS,
                    ));
                }
                version == MODERN
            } else {
                false
            };

        if method == "server/discover" {
            if !params.is_empty() {
                return Err(ErrorData::invalid_params(
                    "server/discover accepts only _meta",
                    None,
                ));
            }
            return Ok(json!(DiscoverResult::from_server_info(
                SUPPORTED_VERSIONS.to_vec(),
                server_config(),
            )));
        }

        if !matches!(method, "tools/list" | "tools/call") {
            return Err(ErrorData::new(
                RpcCode::METHOD_NOT_FOUND,
                "Method not found",
                None,
            ));
        }
        if !modern && self.legacy != LegacyState::Ready {
            return Err(ErrorData::invalid_request(
                "Complete the legacy MCP handshake first",
                None,
            ));
        }

        if method == "tools/list" {
            if params.contains_key("cursor") {
                return Err(ErrorData::invalid_params("Unknown tools/list cursor", None));
            }
            return Ok(json!(list_tools(modern)));
        }

        let mut result = tool_result(tool_request(params)?.and_then(&self.call))?;
        if modern {
            result.meta = Some(server_meta());
        } else {
            result.result_type = None;
        }
        Ok(json!(result))
    }

    fn initialize(&mut self, params: JsonObject) -> Result<Value, ErrorData> {
        if self.legacy != LegacyState::New {
            return Err(ErrorData::invalid_request(
                "MCP is already initialized",
                None,
            ));
        }

        // Validate the legacy handshake before selecting its only supported revision.
        if !["capabilities", "clientInfo"]
            .iter()
            .all(|key| params.get(*key).is_some_and(Value::is_object))
            || serde_json::from_value::<InitializeRequestParams>(Value::Object(params)).is_err()
        {
            return Err(ErrorData::invalid_params(
                "initialize requires protocolVersion, capabilities, and clientInfo",
                None,
            ));
        }
        self.legacy = LegacyState::AwaitingInitialized;
        Ok(json!(server_config()))
    }
}

fn server_info() -> Implementation {
    Implementation::new("overlook", env!("CARGO_PKG_VERSION"))
}

fn server_config() -> InitializeResult {
    let mut capabilities = ServerCapabilities::default();
    capabilities.tools = Some(Default::default());
    InitializeResult::new(capabilities)
        .with_server_info(server_info())
        .with_protocol_version(LEGACY)
}

fn server_meta() -> MetaObject {
    MetaObject::from(JsonObject::from_iter([(
        SERVER_INFO_KEY.to_owned(),
        json!(server_info()),
    )]))
}

fn list_tools(modern: bool) -> ListToolsResult {
    let mut catalog = vec![
        Tool::new(
            ACCOUNTS_LIST,
            "List Overlook accounts visible to this connection.",
            JsonObject::from_iter([
                ("type".into(), json!("object")),
                ("additionalProperties".into(), json!(false)),
            ]),
        ),
        Tool::new(
            ACCOUNT_CAPABILITIES,
            "Get mail, calendar, and contacts availability for an Overlook account.",
            JsonObject::from_iter([
                ("type".into(), json!("object")),
                (
                    "properties".into(),
                    json!({"account_id": {"type": "string", "minLength": 1}}),
                ),
                ("required".into(), json!(["account_id"])),
                ("additionalProperties".into(), json!(false)),
            ]),
        ),
    ];
    catalog.extend(
        read::tools().map(|tool| Tool::new(tool.name, tool.description, tool.schema.clone())),
    );
    for tool in &mut catalog {
        tool.annotations = Some(if tool.name == "mail_refresh" {
            ToolAnnotations::new()
                .read_only(false)
                .destructive(false)
                .open_world(true)
        } else {
            ToolAnnotations::new()
                .read_only(true)
                .open_world(tool.name == "mail_search")
        });
    }
    catalog.extend(write::tools().map(|entry| {
        let mut tool = Tool::new(entry.name, entry.description, entry.schema.clone());
        tool.annotations = Some(
            ToolAnnotations::new()
                .read_only(false)
                .destructive(
                    matches!(
                        entry.permission,
                        write::WritePermission::Send
                            | write::WritePermission::Invite
                            | write::WritePermission::PermanentDelete
                    ) || entry.name.ends_with("_delete"),
                )
                .idempotent(true)
                .open_world(true),
        );
        tool
    }));
    let mut tools = ListToolsResult::with_all_items(catalog);
    if modern {
        tools.ttl_ms = Some(0);
        tools.cache_scope = Some(CacheScope::Private);
        tools.meta = Some(server_meta());
    } else {
        tools.result_type = None;
    }
    tools
}

fn tool_request(mut params: JsonObject) -> Result<Result<Request, Failure>, ErrorData> {
    let Some(Value::String(name)) = params.remove("name") else {
        return Err(ErrorData::invalid_params(
            "tools/call requires a tool name",
            None,
        ));
    };
    if ["requestState", "inputResponses", "task"]
        .iter()
        .any(|key| params.contains_key(*key))
    {
        return Err(ErrorData::invalid_params(
            "This tool supports a single request and result",
            None,
        ));
    }
    let mut arguments = match params.remove("arguments") {
        None => JsonObject::new(),
        Some(Value::Object(arguments)) => arguments,
        Some(_) => {
            return Err(ErrorData::invalid_params(
                "Tool arguments must be an object",
                None,
            ))
        }
    };

    let invalid = |message| Ok(Err(Failure::new(ErrorCode::InvalidRequest, message)));
    match name.as_str() {
        ACCOUNTS_LIST if arguments.is_empty() => Ok(Ok(Request::AccountsList)),
        ACCOUNTS_LIST => invalid("accounts_list takes no arguments"),
        ACCOUNT_CAPABILITIES => match arguments.remove("account_id") {
            Some(Value::String(account_id)) if !account_id.is_empty() && arguments.is_empty() => {
                Ok(Ok(Request::AccountCapabilities { account_id }))
            }
            _ => invalid("account_capabilities requires one non-empty string argument, account_id"),
        },
        _ if write::tools().any(|tool| tool.name == name) => {
            let Some(Value::String(account_id)) = arguments.remove("account_id") else {
                return invalid("Choose one account_id for a write");
            };
            if account_id.is_empty() || account_id.len() > 128 {
                return invalid("Invalid account selector");
            }
            let Some(Value::String(idempotency_key)) = arguments.remove("idempotency_key") else {
                return invalid("Provide an idempotency_key for a write");
            };
            Ok(Ok(Request::Write {
                name,
                account_id,
                idempotency_key,
                args: arguments,
            }))
        }
        _ if read::tools().any(|tool| tool.name == name) => {
            let batch = arguments.contains_key("account_ids");
            let ids = match (
                arguments.remove("account_id"),
                arguments.remove("account_ids"),
            ) {
                (Some(Value::String(id)), None) => vec![id],
                (None, Some(Value::Array(ids))) if read::batch_tool(&name) => {
                    let Some(ids) = ids
                        .into_iter()
                        .map(|id| id.as_str().map(str::to_owned))
                        .collect()
                    else {
                        return invalid("account_ids must contain only opaque account IDs");
                    };
                    ids
                }
                _ => return invalid("Choose an explicit account_id or supported account_ids list"),
            };
            if ids.is_empty()
                || ids.len() > read::MAX_ACCOUNTS
                || ids
                    .iter()
                    .any(|id: &String| id.is_empty() || id.len() > 128)
            {
                return invalid("Invalid account selector");
            }
            Ok(Ok(Request::Read {
                name,
                account_ids: ids,
                batch,
                args: arguments,
            }))
        }
        _ => Err(ErrorData::invalid_params("Unknown tool", None)),
    }
}

fn tool_result(reply: Reply) -> Result<CallToolResult, ErrorData> {
    match reply {
        Ok(response) => serde_json::to_value(response).map(CallToolResult::structured),
        Err(failure) => serde_json::to_value(failure).map(CallToolResult::structured_error),
    }
    .map_err(|_| ErrorData::internal_error("Could not serialize tool result", None))
}

#[cfg(test)]
mod tests;

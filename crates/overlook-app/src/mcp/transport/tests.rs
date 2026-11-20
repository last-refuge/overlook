use std::io::BufReader;

use super::*;
use crate::mcp::contract::{Account, Capabilities, Response};

fn exchange(input: &[u8], call: impl Fn(Request) -> Reply) -> Vec<Value> {
    let mut output = Vec::new();
    serve_stream(BufReader::with_capacity(32, input), &mut output, call).unwrap();
    assert!(output.is_empty() || output.ends_with(b"\n"));
    output
        .lines()
        .map(|line| {
            let response: Value = serde_json::from_str(&line.unwrap()).unwrap();
            assert_eq!(response["jsonrpc"], "2.0");
            response
        })
        .collect()
}

fn exchange_json(messages: &[Value], call: impl Fn(Request) -> Reply) -> Vec<Value> {
    let mut input = Vec::new();
    for message in messages {
        serde_json::to_writer(&mut input, message).unwrap();
        input.push(b'\n');
    }
    exchange(&input, call)
}

fn modern_method(id: u32, method: &str, params: Value) -> Value {
    let mut request = json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params});
    request["params"]["_meta"] = json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientCapabilities": {}
    });
    request
}

fn legacy_initialize(version: &str) -> Value {
    json!({"jsonrpc":"2.0", "id":"init", "method":"initialize", "params":{
        "protocolVersion":version,
        "capabilities":{},
        "clientInfo":{"name":"test-client", "version":"1"}
    }})
}

fn sample_call(request: Request) -> Reply {
    match request {
        Request::AccountsList => Ok(Response::Accounts {
            accounts: vec![Account {
                account_id: "sample".into(),
                display_name: "Sample Mailbox".into(),
                email: "sample@example.com".into(),
                provider: "sample".into(),
            }],
        }),
        Request::AccountCapabilities { account_id } if account_id == "sample" => {
            Ok(Response::Capabilities(Capabilities {
                account_id,
                mail: true,
                calendar: false,
                contacts: false,
            }))
        }
        Request::AccountCapabilities { .. } => {
            Err(Failure::new(ErrorCode::AccessDenied, "Account unavailable"))
        }
    }
}

fn should_not_call(_: Request) -> Reply {
    panic!("unexpected contract call")
}

#[test]
fn modern_tools_work_in_a_fresh_process_after_a_separate_discovery_probe() {
    let probe = exchange_json(
        &[modern_method(1, "server/discover", json!({}))],
        should_not_call,
    );
    let discovery = &probe[0]["result"];
    assert_eq!(
        discovery["supportedVersions"],
        json!(["2026-07-28", "2025-11-25"])
    );
    assert_eq!(discovery["resultType"], "complete");
    assert_eq!(discovery["capabilities"], json!({"tools":{}}));
    assert_eq!(discovery["ttlMs"], 0);
    assert_eq!(discovery["cacheScope"], "private");
    assert_eq!(discovery["_meta"][SERVER_INFO_KEY]["name"], "overlook");

    let responses = exchange_json(
        &[
            modern_method(2, "tools/list", json!({})),
            modern_method(3, "tools/call", json!({"name": "accounts_list"})),
            modern_method(
                4,
                "tools/call",
                json!({
                    "name": "account_capabilities", "arguments": {"account_id": "sample"}
                }),
            ),
            modern_method(
                5,
                "tools/call",
                json!({
                    "name": "account_capabilities", "arguments": {"account_id": "missing"}
                }),
            ),
        ],
        sample_call,
    );

    let list = &responses[0]["result"];
    assert_eq!(list["ttlMs"], 0);
    assert_eq!(list["cacheScope"], "private");
    assert!(list.get("nextCursor").is_none());
    let tools = list["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0]["name"], "accounts_list");
    assert_eq!(tools[0]["inputSchema"]["additionalProperties"], false);
    assert_eq!(tools[1]["name"], "account_capabilities");
    assert_eq!(tools[1]["inputSchema"]["required"], json!(["account_id"]));

    for response in &responses {
        assert_eq!(response["result"]["resultType"], "complete");
        assert_eq!(
            response["result"]["_meta"][SERVER_INFO_KEY]["name"],
            "overlook"
        );
    }
    for response in &responses[1..] {
        let result = &response["result"];
        assert_eq!(result["content"][0]["type"], "text");
        let fallback = result["content"][0]["text"].as_str().unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(fallback).unwrap(),
            result["structuredContent"]
        );
    }
    assert_eq!(responses[1]["result"]["isError"], false);
    assert_eq!(
        responses[1]["result"]["structuredContent"]["accounts"][0]["account_id"],
        "sample"
    );
    assert_eq!(
        responses[2]["result"]["structuredContent"],
        json!({
            "account_id":"sample", "mail":true, "calendar":false, "contacts":false
        })
    );
    assert_eq!(responses[3]["result"]["isError"], true);
    assert_eq!(
        responses[3]["result"]["structuredContent"],
        json!({
            "code":"access_denied", "message":"Account unavailable"
        })
    );
}

#[test]
fn legacy_handshake_consumes_notification_and_uses_legacy_result_shapes() {
    let responses = exchange_json(
        &[
            json!({"jsonrpc":"2.0", "id":0, "method":"ping"}),
            legacy_initialize("2025-11-25"),
            json!({"jsonrpc":"2.0", "id":1, "method":"tools/list"}),
            json!({"jsonrpc":"2.0", "method":"notifications/initialized"}),
            json!({"jsonrpc":"2.0", "id":2, "method":"tools/list"}),
            json!({"jsonrpc":"2.0", "id":3, "method":"tools/call", "params":{"name":"accounts_list","arguments":{}}}),
            json!({"jsonrpc":"2.0", "id":4, "method":"tools/call", "params":{
                "name":"account_capabilities", "arguments":{"account_id":"missing"}
            }}),
        ],
        sample_call,
    );
    assert_eq!(responses.len(), 6);
    assert_eq!(responses[0]["result"], json!({}));
    assert_eq!(responses[1]["id"], "init");
    assert_eq!(responses[1]["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(responses[1]["result"]["serverInfo"]["name"], "overlook");
    assert_eq!(responses[2]["error"]["code"], -32600);
    assert_eq!(responses[3]["id"], 2);
    assert_eq!(responses[3]["result"]["tools"].as_array().unwrap().len(), 2);
    for response in [&responses[3], &responses[4], &responses[5]] {
        assert!(response["result"].get("resultType").is_none());
        assert!(response["result"].get("ttlMs").is_none());
        assert!(response["result"].get("cacheScope").is_none());
    }
    assert_eq!(responses[4]["result"]["isError"], false);
    assert_eq!(responses[5]["result"]["isError"], true);
    assert_eq!(
        responses[5]["result"]["structuredContent"]["code"],
        "access_denied"
    );
}

#[test]
fn modern_version_and_metadata_are_checked_on_each_request() {
    let mut unsupported = modern_method(1, "server/discover", json!({}));
    unsupported["params"]["_meta"][VERSION_KEY] = json!("2099-01-01");
    let mut malformed = modern_method(3, "tools/list", json!({}));
    malformed["params"]["_meta"]["io.modelcontextprotocol/clientCapabilities"] = json!(false);

    let responses = exchange_json(
        &[
            unsupported,
            modern_method(2, "server/discover", json!({})),
            malformed,
            json!({"jsonrpc":"2.0", "id":4, "method":"tools/list"}),
            modern_method(5, "initialize", json!({})),
            modern_method(6, "ping", json!({})),
            legacy_initialize("2026-07-28"),
        ],
        should_not_call,
    );
    assert_eq!(responses[0]["error"]["code"], -32022);
    assert_eq!(
        responses[0]["error"]["data"]["supported"],
        json!(["2026-07-28", "2025-11-25"])
    );
    assert_eq!(responses[0]["error"]["data"]["requested"], "2099-01-01");
    assert_eq!(responses[1]["result"]["resultType"], "complete");
    for (response, code) in responses[2..6].iter().zip([-32602, -32602, -32601, -32601]) {
        assert_eq!(response["error"]["code"], code);
    }
    assert_eq!(responses[6]["result"]["protocolVersion"], "2025-11-25");
}

#[test]
fn malformed_tool_calls_and_argument_schema_violations_get_proper_errors() {
    let invalid_requests = [
        json!({}),
        json!({"name": "absent"}),
        json!({"name": "accounts_list", "arguments": null}),
        json!({"name": "accounts_list", "arguments": []}),
    ];
    let invalid_arguments = [
        json!({"name": "accounts_list", "arguments": {"extra": true}}),
        json!({"name": "account_capabilities"}),
        json!({"name": "account_capabilities", "arguments": {"account_id": 1}}),
        json!({"name": "account_capabilities", "arguments": {"account_id": ""}}),
        json!({"name": "account_capabilities", "arguments": {"account_id": "sample", "extra": true}}),
    ];
    let messages: Vec<_> = invalid_requests
        .iter()
        .chain(&invalid_arguments)
        .enumerate()
        .map(|(id, params)| modern_method(id as u32, "tools/call", params.clone()))
        .collect();
    let responses = exchange_json(&messages, should_not_call);
    for (id, response) in responses.iter().enumerate() {
        assert_eq!(response["id"], id);
        if id < invalid_requests.len() {
            assert_eq!(response["error"]["code"], -32602);
        } else {
            assert_eq!(response["result"]["isError"], true);
            assert_eq!(
                response["result"]["structuredContent"]["code"],
                "invalid_request"
            );
        }
    }
}

#[test]
fn malformed_or_oversized_frames_do_not_poison_the_stream() {
    let mut input = b"{malformed}\n[]\n".to_vec();
    input.extend_from_slice(b"{\"jsonrpc\":\"1.0\",\"id\":2,\"method\":\"ping\"}\n");
    input.extend_from_slice(b"{\"jsonrpc\":\"2.0\",\"id\":null,\"method\":\"ping\"}\n");
    input.extend_from_slice(b"{\"jsonrpc\":\"2.0\",\"id\":4,\"method\":\"ping\",\"params\":[]}\n");
    input.extend(std::iter::repeat_n(b'x', 2 * MAX_FRAME_BYTES));
    input.extend_from_slice(b"\n{\"jsonrpc\":\"2.0\",\"id\":5,\"method\":\"ping\"}\r\n");
    input.extend_from_slice(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/cancelled\"}\n");
    input.extend_from_slice(b"{\"jsonrpc\":");
    let responses = exchange(&input, should_not_call);
    assert_eq!(responses.len(), 8);
    for (index, code) in [
        (0, -32700),
        (1, -32600),
        (2, -32600),
        (3, -32600),
        (4, -32602),
        (5, -32600),
        (7, -32700),
    ] {
        assert_eq!(responses[index]["error"]["code"], code);
    }
    assert_eq!(responses[2]["id"], 2);
    assert!(responses[3].get("id").is_none());
    assert_eq!(responses[4]["id"], 4);
    assert_eq!(responses[6]["id"], 5);
    assert_eq!(responses[6]["result"], json!({}));
}

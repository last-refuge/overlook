use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{symlink, PermissionsExt};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use overlook_provider::sample::SampleProvider;
use overlook_store::Store;
use serde_json::json;

use super::super::transport::{LEGACY, MODERN, VERSION_KEY};
use super::*;

pub(super) const ALLOWED: &str = "https://client.example";
const HOST: &str = "127.0.0.1:8765";

fn test_token(byte: u8) -> String {
    URL_SAFE_NO_PAD.encode([byte; 32])
}

fn config(token: &str) -> Value {
    json!({
        "grant_client": "fixture-bridge",
        "grant_token": token,
        "allowed_origins": [ALLOWED],
        "auth": {"mode": "loopback", "bearer_token": test_token(2)}
    })
}

#[test]
fn configuration_requires_private_file_and_numeric_loopback() {
    let mut config_file = tempfile::NamedTempFile::new().unwrap();
    write!(config_file, "{}", config(&test_token(1))).unwrap();
    assert!(Config::read(config_file.path()).is_ok());
    fs::set_permissions(config_file.path(), fs::Permissions::from_mode(0o640)).unwrap();
    assert!(Config::read(config_file.path()).is_err());
    let dir = tempfile::tempdir().unwrap();
    let alias = dir.path().join("alias");
    symlink(config_file.path(), &alias).unwrap();
    assert!(Config::read(&alias).is_err());

    let mut args = vec!["--config".into(), "fixture.json".into()];
    assert_eq!(options(&args).unwrap().0.to_string(), HOST);
    args.push("--bind".into());
    args.push(String::new());
    for rejected in ["localhost:8765", "0.0.0.0:8765", "[::]:8765", "127.0.0.1:0"] {
        args[3] = rejected.to_owned();
        assert!(options(&args).is_err(), "{rejected}");
    }
    args[3] = "[::1]:8765".into();
    assert!(options(&args).is_ok());
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

pub(super) fn rpc(id: u64, method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

fn modern_rpc(method: &str, name: Option<&str>) -> Value {
    let mut params = json!({"_meta": {
        VERSION_KEY: MODERN.as_str(),
        "io.modelcontextprotocol/clientCapabilities": {},
    }});
    if let Some(name) = name {
        params["name"] = json!(name);
    }
    rpc(1, method, params)
}

#[test]
fn streamable_http_reuses_live_owner_and_grant() {
    let rt = runtime();
    let profile = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(&profile.path().join("sample.sqlite")).unwrap());
    crate::seed_sample_account(&store).unwrap();
    let _owner = service::start(profile.path(), store, SampleProvider::new()).unwrap();
    let grant_token = service::grant(
        profile.path(),
        "fixture-bridge",
        &["sample".into()],
        BTreeSet::new(),
    )
    .unwrap();
    let config: Config = serde_json::from_value(config(&grant_token)).unwrap();
    let Config {
        grant_client,
        grant_token,
        allowed_origins,
        auth,
    } = config;
    let path = profile.path().to_path_buf();
    let listener = rt.block_on(TcpListener::bind("127.0.0.1:0")).unwrap();
    let host = listener.local_addr().unwrap().to_string();
    let url = format!("http://{host}{MCP_PATH}");
    let bridge = Arc::new(Bridge {
        host,
        allowed_origins,
        auth: Auth::new(auth, &grant_token).unwrap(),
        grant_token: grant_token.clone(),
        call: Box::new(move |request| service::call(&path, &grant_client, &grant_token, request)),
        active_calls: Arc::new(Semaphore::new(MAX_ACTIVE_CALLS)),
    });
    let server = rt.spawn(serve(listener, bridge));
    let agent = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .proxy(None)
            .http_status_as_error(false)
            .timeout_global(Some(READ_TIMEOUT))
            .build(),
    );
    let auth_header = format!("Bearer {}", test_token(2));
    let modern = |method, name| {
        let mut request = agent
            .post(&url)
            .header("Authorization", &auth_header)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .header("Origin", ALLOWED)
            .header("MCP-Protocol-Version", MODERN.as_str())
            .header("Mcp-Method", method);
        if let Some(name) = name {
            request = request.header("Mcp-Name", name);
        }
        request
    };
    let legacy = || {
        agent
            .post(&url)
            .header("Authorization", &auth_header)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
    };

    let mut discover = modern("server/discover", None)
        .send_json(modern_rpc("server/discover", None))
        .unwrap();
    assert_eq!(discover.status(), 200);
    assert_eq!(discover.headers()["cache-control"], "no-store");
    assert!(discover.headers().get("mcp-session-id").is_none());
    let discover: Value = discover.body_mut().read_json().unwrap();
    assert_eq!(
        discover["result"]["supportedVersions"],
        json!([MODERN, LEGACY])
    );

    let call = modern_rpc("tools/call", Some("accounts_list"));
    let accounts: Value = modern("tools/call", Some("accounts_list"))
        .send_json(&call)
        .unwrap()
        .body_mut()
        .read_json()
        .unwrap();
    let accounts = &accounts["result"]["structuredContent"]["accounts"];
    assert_eq!(accounts.as_array().unwrap().len(), 1);
    assert_eq!(accounts[0]["email"], "jason@example.com");

    let mut initialize = legacy()
        .send_json(rpc(
            2,
            "initialize",
            json!({
                "protocolVersion": LEGACY.as_str(),
                "capabilities": {},
                "clientInfo": {"name":"fixture", "version":"1"}
            }),
        ))
        .unwrap();
    assert_eq!(initialize.status(), 200);
    assert!(initialize.headers().get("mcp-session-id").is_none());
    let initialize: Value = initialize.body_mut().read_json().unwrap();
    assert_eq!(initialize["result"]["protocolVersion"], LEGACY.as_str());
    let mut accepted = legacy()
        .header("MCP-Protocol-Version", LEGACY.as_str())
        .send_json(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
        .unwrap();
    assert_eq!(accepted.status(), 202);
    assert!(accepted.body_mut().read_to_vec().unwrap().is_empty());
    let legacy_list: Value = legacy()
        .header("MCP-Protocol-Version", LEGACY.as_str())
        .send_json(rpc(3, "tools/list", json!({})))
        .unwrap()
        .body_mut()
        .read_json()
        .unwrap();
    assert!(legacy_list["result"]["tools"].as_array().unwrap().len() > 2);

    for method in ["GET", "DELETE"] {
        let response = agent
            .run(
                ureq::http::Request::builder()
                    .uri(&url)
                    .method(method)
                    .header("Authorization", &auth_header)
                    .body(())
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(response.status(), 405);
        assert_eq!(response.headers()["allow"], "POST");
    }
    let response = agent.get(&url).call().unwrap();
    assert_eq!(response.status(), 401);

    let mut mismatch = modern("tools/list", None).send_json(&call).unwrap();
    assert_eq!(mismatch.status(), 400);
    let mismatch: Value = mismatch.body_mut().read_json().unwrap();
    assert_eq!(mismatch["error"]["code"], -32020);
    let mut unsupported = modern("future/method", None)
        .send_json(modern_rpc("future/method", None))
        .unwrap();
    assert_eq!(unsupported.status(), 404);
    let unsupported: Value = unsupported.body_mut().read_json().unwrap();
    assert_eq!(unsupported["error"]["code"], -32601);
    let too_large = modern("tools/list", None)
        .send(" ".repeat(MAX_BODY_BYTES + 1).as_bytes())
        .unwrap();
    assert_eq!(too_large.status(), 413);

    service::revoke(profile.path(), "fixture-bridge").unwrap();
    let result: Value = modern("tools/call", Some("accounts_list"))
        .send_json(&call)
        .unwrap()
        .body_mut()
        .read_json()
        .unwrap();
    assert_eq!(
        result["result"]["structuredContent"]["code"],
        "access_denied"
    );
    server.abort();
}

#[test]
fn host_origin_and_token_protect_owner_calls() {
    let rt = runtime();
    let grant_token = test_token(1);
    let bridge = Arc::new(Bridge {
        host: HOST.into(),
        allowed_origins: vec![ALLOWED.into()],
        auth: Auth::new(
            auth::Settings::Loopback {
                bearer_token: test_token(2),
            },
            &grant_token,
        )
        .unwrap(),
        grant_token: grant_token.clone(),
        call: Box::new(|_| panic!("Denied requests cannot call the profile owner")),
        active_calls: Arc::new(Semaphore::new(1)),
    });
    let request = || {
        Request::builder()
            .method(Method::POST)
            .uri(MCP_PATH)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", MODERN.as_str())
            .header("Mcp-Method", "tools/call")
            .header("Mcp-Name", "accounts_list")
    };
    let body = || {
        Full::new(Bytes::from(
            modern_rpc("tools/call", Some("accounts_list")).to_string(),
        ))
    };
    let status = |request| rt.block_on(bridge.clone().handle(request)).status();
    let valid_auth = format!("Bearer {}", test_token(2));
    assert_eq!(
        status(
            request()
                .header("Host", "localhost:8765")
                .header("Authorization", &valid_auth)
                .body(body())
                .unwrap()
        ),
        421
    );
    assert_eq!(
        status(
            request()
                .header("Host", HOST)
                .header("Host", HOST)
                .header("Authorization", &valid_auth)
                .body(body())
                .unwrap()
        ),
        421
    );
    for origin in [
        "null",
        "https://other.example",
        "https://client.example.evil.example",
    ] {
        assert_eq!(
            status(
                request()
                    .header("Host", HOST)
                    .header("Origin", origin)
                    .header("Authorization", &valid_auth)
                    .body(body())
                    .unwrap()
            ),
            403
        );
    }
    assert_eq!(
        status(
            request()
                .header("Host", HOST)
                .header("Origin", ALLOWED)
                .header("Origin", ALLOWED)
                .header("Authorization", &valid_auth)
                .body(body())
                .unwrap()
        ),
        403
    );
    assert_eq!(
        status(
            request()
                .header("Host", HOST)
                .header("Authorization", format!("Bearer {grant_token}"))
                .body(body())
                .unwrap()
        ),
        401
    );
}

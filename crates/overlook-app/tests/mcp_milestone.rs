use serde_json::{json, Value};
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::{NamedTempFile, TempDir};

const MODERN: &str = "2026-07-28";
const LEGACY: &str = "2025-11-25";
const PRIMARY: &str = "sample";
const SECONDARY: &str = "sample-secondary";
const TIMEOUT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(20);

#[test]
fn modern_and_legacy_clients_share_one_owner_with_live_grant_checks() {
    let root = fixture();
    let profile = root.path().join("profile");
    let alpha = grant(&profile, "alpha", &[PRIMARY]);
    let mut owner = start_owner(&profile);
    let beta = grant(&profile, "beta", &[PRIMARY, SECONDARY]);
    let alias = root.path().join("alias");
    symlink(&profile, &alias).unwrap();

    let mut modern = Mcp::start(&profile, "alpha", &alpha, MODERN);
    let discover = modern.request("server/discover", json!({}));
    assert!(discover["supportedVersions"]
        .as_array()
        .expect("server/discover supportedVersions")
        .contains(&json!(MODERN)));
    assert!(discover["capabilities"]["tools"].is_object());
    assert_tool_catalog(&mut modern);

    let mut legacy = Mcp::start(&alias, "alpha", &alpha, LEGACY);
    assert_tool_catalog(&mut legacy);
    let primary = only_primary(&modern.accounts());
    assert_eq!(only_primary(&legacy.accounts()), primary);
    assert_capabilities(&mut modern, &primary);
    assert_capabilities(&mut legacy, &primary);

    let mut both = Mcp::start(&profile, "beta", &beta, MODERN);
    let accounts = both.accounts();
    assert_eq!(
        accounts.len(),
        2,
        "the repeated grant should expose both accounts"
    );
    let beta_primary = account_id(&accounts, "Jason Alexander", "jason@example.com");
    let secondary = account_id(&accounts, "Sample Secondary", "secondary@example.com");
    assert_eq!(beta_primary, primary);
    assert_ne!(primary, secondary);
    assert_capabilities(&mut both, &secondary);
    assert_failure(
        &modern.tool("account_capabilities", json!({"account_id": secondary})),
        "access_denied",
    );
    assert_failure(
        &legacy.tool("account_capabilities", json!({"account_id": secondary})),
        "access_denied",
    );

    let mut duplicate = Process::spawn(command(&alias, &["mcp", "sample"]));
    let status = duplicate.wait();
    assert!(
        status.code().is_some_and(|code| code != 0),
        "expected the duplicate owner to be rejected, got {status}: {}",
        duplicate.diagnostics()
    );
    owner.assert_running();
    assert_eq!(only_primary(&modern.accounts()), primary);
    assert_eq!(only_primary(&legacy.accounts()), primary);

    Process::spawn(command(&profile, &["mcp", "revoke", "--client", "alpha"])).assert_success();
    assert_failure(&modern.tool("accounts_list", json!({})), "access_denied");
    assert_failure(
        &legacy.tool("account_capabilities", json!({"account_id": primary})),
        "access_denied",
    );
    assert_capabilities(&mut both, &secondary);

    modern.finish();
    legacy.finish();
    both.finish();
    quit_owner(&alias, &mut owner);
}

#[test]
fn wrong_profile_and_stopped_profile_have_distinct_tool_errors() {
    let root = fixture();
    let a = root.path().join("profile-a");
    let b = root.path().join("profile-b");
    let mut owner_a = start_owner(&a);
    let mut owner_b = start_owner(&b);
    let token_a = grant(&a, "alpha", &[PRIMARY]);
    let token_b = grant(&b, "alpha", &[PRIMARY]);
    assert!(
        token_a != token_b,
        "the profiles issued the same grant token"
    );

    let mut client_a = Mcp::start(&a, "alpha", &token_a, MODERN);
    let mut client_b = Mcp::start(&b, "alpha", &token_b, MODERN);
    let account_a = only_primary(&client_a.accounts());
    let account_b = only_primary(&client_b.accounts());
    assert_ne!(account_a, account_b, "account IDs must be profile-local");

    let mut wrong_profile = Mcp::start(&b, "alpha", &token_a, MODERN);
    assert_failure(
        &wrong_profile.tool("accounts_list", json!({})),
        "access_denied",
    );
    wrong_profile.finish();
    only_primary(&client_b.accounts());

    quit_owner(&b, &mut owner_b);
    let mut stopped = Mcp::start(&b, "alpha", &token_b, MODERN);
    assert_failure(
        &stopped.tool("accounts_list", json!({})),
        "profile_unavailable",
    );
    only_primary(&client_a.accounts());

    client_a.finish();
    client_b.finish();
    stopped.finish();
    quit_owner(&a, &mut owner_a);
}

#[test]
fn sample_mode_leaves_existing_desktop_database_alone() {
    let root = fixture();
    let profile = root.path().join("profile");
    fs::create_dir(&profile).unwrap();
    let database = profile.join("overlook.sqlite");
    let contents = b"existing profile data";
    fs::write(&database, contents).unwrap();

    let mut process = Process::spawn(command(&profile, &["mcp", "sample"]));
    assert!(!process.wait().success());
    assert!(process
        .diagnostics()
        .contains("without an Overlook database"));
    assert_eq!(fs::read(database).unwrap(), contents);
}

#[test]
fn batch_reads_authorize_each_account_and_keep_results_separate() {
    let root = fixture();
    let profile = root.path().join("profile");
    let mut owner = start_owner(&profile);
    let full = grant(&profile, "full", &[PRIMARY, SECONDARY]);
    let limited = grant(&profile, "limited", &[PRIMARY]);
    let mut client = Mcp::start(&profile, "full", &full, MODERN);
    let accounts = client.accounts();
    let primary = account_id(&accounts, "Jason Alexander", "jason@example.com");
    let secondary = account_id(&accounts, "Sample Secondary", "secondary@example.com");
    let batch = json!({"account_ids":[primary, secondary],"query":"Alex"});
    let result = client.tool("contacts_search", batch.clone());
    let results = success(&result)["results"].as_array().unwrap();
    assert_eq!(results[0]["account_id"], primary);
    assert_eq!(results[1]["account_id"], secondary);
    let alex = &results[1]["items"][0];
    assert_eq!(alex["display_name"], "Alex Rivera");
    assert_eq!(alex["email"], "alex@studio.example");
    let record = json!({"account_id":primary,"contact_id":alex["contact_id"]});
    assert_failure(&client.tool("contacts_get", record), "access_denied");

    let address_book = &alex["address_book_id"];
    let scoped = client.tool(
        "contacts_search",
        json!({
            "account_ids":[primary, secondary], "query":"Alex", "address_book_id":address_book
        }),
    );
    let scoped = success(&scoped);
    assert_eq!(scoped["partial"], true);
    assert_eq!(scoped["results"][0]["error"]["code"], "access_denied");
    assert_eq!(
        scoped["results"][1]["items"][0]["contact_id"],
        alex["contact_id"]
    );

    let mut legacy = Mcp::start(&profile, "limited", &limited, LEGACY);
    assert_failure(&legacy.tool("contacts_search", batch), "access_denied");
    let one = legacy.tool(
        "contacts_search",
        json!({"account_ids":[primary],"query":"Daniel"}),
    );
    assert_eq!(success(&one)["results"][0]["account_id"], primary);
    client.finish();
    legacy.finish();
    quit_owner(&profile, &mut owner);
}

#[test]
fn cross_account_reads_join_mail_calendar_and_contacts_and_report_coverage() {
    use overlook_core::{time::now, tz};

    let root = fixture();
    let profile = root.path().join("profile");
    let mut owner = start_owner(&profile);
    let token = grant(&profile, "reader", &[PRIMARY, SECONDARY]);
    let mut client = Mcp::start(&profile, "reader", &token, MODERN);
    let accounts = client.accounts();
    let primary = account_id(&accounts, "Jason Alexander", "jason@example.com");
    let secondary = account_id(&accounts, "Sample Secondary", "secondary@example.com");

    let search = client.read(
        "mail_search",
        json!({"account_id":primary,"query":"Trail walk"}),
    );
    assert_eq!(search["cache"]["freshness"], "stale");
    assert_eq!(search["source"], "cache");
    let invitation = search["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["subject"] == "Invitation: Trail walk")
        .unwrap();
    let message = client.read(
        "mail_message_get",
        json!({"account_id":primary,"message_id":invitation["message_id"]}),
    );
    assert_eq!(message["from"]["email"], "sophie@example.com");
    assert!(message["body_text"]
        .as_str()
        .unwrap()
        .contains("ridge trailhead"));
    assert_eq!(message["content_is_untrusted"], true);

    let now = now();
    let start = tz::to_rfc3339(now - 8 * 86_400_000);
    let end = tz::to_rfc3339(now + 15 * 86_400_000);
    let calendar = client.read(
        "calendar_events_list",
        json!({
            "account_id":secondary,"start":start,"end":end,"query":"Trail walk"
        }),
    );
    assert_eq!(calendar["coverage"]["status"], "partial");
    assert!(!calendar["coverage"]["gaps"].as_array().unwrap().is_empty());
    let event = &calendar["items"][0];
    assert_eq!(event["title"], "Trail walk");
    let details = client.read(
        "calendar_event_get",
        json!({"account_id":secondary,"event_id":event["event_id"]}),
    );
    assert_eq!(details["title"], event["title"]);
    assert_eq!(details["location"], "Ridge trailhead");
    assert!(details["start"].as_str().unwrap().ends_with('Z'));
    assert_eq!(message["invitation"]["summary"], details["title"]);
    assert_eq!(
        tz::to_rfc3339(message["invitation"]["start_ms"].as_i64().unwrap()),
        details["start"]
    );

    let contact = client.read(
        "contacts_search",
        json!({
            "account_id":secondary,"query":message["from"]["email"]
        }),
    );
    assert_eq!(contact["items"][0]["display_name"], "Sophie Williams");
    assert_eq!(calendar["account_id"], secondary);
    assert_eq!(message["account_id"], primary);

    let availability = client.read(
        "calendar_availability",
        json!({
            "account_id":secondary, "start":start, "end":end
        }),
    );
    let statuses: Vec<_> = availability["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|entry| entry["status"].as_str())
        .collect();
    assert!(statuses.contains(&"unknown"));
    assert!(statuses.contains(&"busy"));

    let provider = client.read(
        "mail_search",
        json!({
            "account_id":primary,"query":"Trail walk","source":"provider"
        }),
    );
    assert_eq!(provider["source"], "provider");
    assert_eq!(provider["coverage"]["provider"]["scope"], "sample_mailbox");
    assert!(provider["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|hit| hit["message_id"] == message["message_id"]));
    client.finish();
    quit_owner(&profile, &mut owner);
}

#[test]
fn mail_read_pagination_and_refresh_keep_message_flags_and_outbox() {
    use base64::{engine::general_purpose::STANDARD, Engine};

    let root = fixture();
    let profile = root.path().join("profile");
    let mut owner = start_owner(&profile);
    let token = grant(&profile, "reader", &[PRIMARY, SECONDARY]);
    let mut client = Mcp::start(&profile, "reader", &token, LEGACY);
    let accounts = client.accounts();
    let primary = account_id(&accounts, "Jason Alexander", "jason@example.com");
    let secondary = account_id(&accounts, "Sample Secondary", "secondary@example.com");

    let first = client.read("mail_folders_list", json!({"account_id":primary,"limit":1}));
    let list = client.read(
        "mail_messages_list",
        json!({
            "account_id":primary,"folder_id":first["items"][0]["folder_id"],"limit":1
        }),
    );
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    assert!(list["next_cursor"].is_string());
    let cursor = &first["next_cursor"];
    let next = client.read(
        "mail_folders_list",
        json!({"account_id":primary,"limit":1,"cursor":cursor}),
    );
    assert_ne!(
        first["items"][0]["folder_id"],
        next["items"][0]["folder_id"]
    );
    assert_failure(
        &client.tool(
            "mail_folders_list",
            json!({
                "account_id":secondary,"limit":1,"cursor":cursor
            }),
        ),
        "invalid_request",
    );

    let search = client.read(
        "mail_search",
        json!({"account_id":primary,"query":"creative final review"}),
    );
    let row = search["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["subject"] == "Launch creative — final review")
        .unwrap();
    let selector = json!({"account_id":primary, "message_id":row["message_id"]});
    let before = client.read("mail_message_get", selector.clone());
    let attachment = before["attachments"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["filename"] == "sample-notes.txt")
        .unwrap();
    let bytes = client.read("mail_attachment_get", json!({
        "account_id":primary, "message_id":row["message_id"], "attachment_id":attachment["attachment_id"]
    }));
    assert_eq!(bytes["encoding"], "base64");
    let decoded = STANDARD.decode(bytes["content"].as_str().unwrap()).unwrap();
    assert!(decoded.starts_with(b"Overlook sample mailbox attachment."));
    let after = client.read("mail_message_get", selector);
    assert_eq!(before["is_read"], row["is_read"]);
    assert_eq!(after["is_read"], before["is_read"]);
    assert_eq!(after["is_starred"], before["is_starred"]);

    let outbox = client.read("mail_outbox_list", json!({"account_id":primary}));
    assert_eq!(outbox["items"][0]["state"], "queued");
    let refresh = client.read("mail_refresh", json!({"account_id":primary}));
    assert_eq!(refresh["status"], "updated_cache");
    assert_eq!(refresh["cache"]["freshness"], "recent");
    let unchanged = client.read("mail_outbox_list", json!({"account_id":primary}));
    assert_eq!(unchanged["items"], outbox["items"]);
    client.finish();
    quit_owner(&profile, &mut owner);
}

fn assert_tool_catalog(client: &mut Mcp) {
    let catalog = client.request("tools/list", json!({}));
    let tools = catalog["tools"].as_array().expect("tools/list tools array");
    for name in ["accounts_list", "account_capabilities"] {
        let tool = tools
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap_or_else(|| panic!("missing {name} in {catalog}"));
        assert_eq!(tool["inputSchema"]["type"], "object");
        if name == "account_capabilities" {
            assert_eq!(
                tool["inputSchema"]["properties"]["account_id"]["type"],
                "string"
            );
            assert!(tool["inputSchema"]["required"]
                .as_array()
                .unwrap()
                .contains(&json!("account_id")));
        }
    }
}

fn assert_capabilities(client: &mut Mcp, id: &str) {
    let result = client.tool("account_capabilities", json!({"account_id": id}));
    let payload = success(&result);
    assert_eq!(payload["account_id"], id);
    for feature in ["mail", "calendar", "contacts"] {
        assert_eq!(
            payload[feature], true,
            "missing Sample capability {feature}"
        );
    }
}

fn only_primary(accounts: &[Value]) -> String {
    assert_eq!(
        accounts.len(),
        1,
        "expected a primary-only grant: {accounts:?}"
    );
    account_id(accounts, "Jason Alexander", "jason@example.com").to_owned()
}

fn account_id<'a>(accounts: &'a [Value], name: &str, email: &str) -> &'a str {
    let account = accounts
        .iter()
        .find(|account| account["email"] == email)
        .unwrap_or_else(|| panic!("missing account {email} in {accounts:?}"));
    assert_eq!(account["display_name"], name);
    assert_eq!(account["provider"], "sample");
    let id = account["account_id"].as_str().expect("string account_id");
    assert!(!id.trim().is_empty() && ![PRIMARY, SECONDARY, name, email].contains(&id));
    id
}

fn success(result: &Value) -> &Value {
    assert!(
        result.get("isError").is_none() || result["isError"] == false,
        "{result}"
    );
    let payload = &result["structuredContent"];
    assert!(
        payload.is_object(),
        "missing structured tool payload in {result}"
    );
    let content = result["content"].as_array().expect("tool content array");
    assert!(
        content.iter().any(|block| {
            block["type"] == "text"
                && block["text"]
                    .as_str()
                    .and_then(|text| serde_json::from_str::<Value>(text).ok())
                    .as_ref()
                    == Some(payload)
        }),
        "missing equivalent JSON text fallback in {result}"
    );
    payload
}

fn assert_failure(result: &Value, code: &str) {
    assert_eq!(result["isError"], true, "{result}");
    assert_eq!(result["structuredContent"]["code"], code, "{result}");
}

struct Mcp {
    process: Process,
    input: ChildStdin,
    output: Receiver<io::Result<String>>,
    version: &'static str,
    next_id: u64,
}

impl Mcp {
    fn start(profile: &Path, client: &str, token: &str, version: &'static str) -> Self {
        let mut command = command(profile, &["mcp", "serve", "--client", client]);
        command
            .env("OVERLOOK_MCP_TOKEN", token)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        let mut process = Process::spawn(command);
        let input = process.child.stdin.take().unwrap();
        let mut reader = BufReader::new(process.child.stdout.take().unwrap());
        let (sender, output) = mpsc::sync_channel(1);
        thread::spawn(move || loop {
            let mut line = String::new();
            match reader.by_ref().take(64 * 1024).read_line(&mut line) {
                Ok(0) => break,
                Ok(_) if line.ends_with('\n') => {
                    if sender.send(Ok(line)).is_err() {
                        break;
                    }
                }
                Ok(_) => {
                    let _ = sender.send(Err(io::Error::other("unterminated or oversized frame")));
                    break;
                }
                Err(error) => {
                    let _ = sender.send(Err(error));
                    break;
                }
            }
        });

        let mut mcp = Self {
            process,
            input,
            output,
            version,
            next_id: 1,
        };
        if version == LEGACY {
            let initialized = mcp.request(
                "initialize",
                json!({
                    "protocolVersion": LEGACY,
                    "capabilities": {},
                    "clientInfo": {"name": "overlook-milestone-tests", "version": "1.0.0"}
                }),
            );
            assert_eq!(initialized["protocolVersion"], LEGACY);
            assert!(initialized["capabilities"]["tools"].is_object());
            assert!(initialized["serverInfo"]["name"].is_string());
            mcp.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        }
        mcp
    }

    fn send(&mut self, message: &Value) {
        writeln!(self.input, "{message}")
            .unwrap_or_else(|error| panic!("{}: {error}", self.process.diagnostics()));
    }

    fn request(&mut self, method: &str, mut params: Value) -> Value {
        if self.version == MODERN {
            params["_meta"] = json!({
                "io.modelcontextprotocol/protocolVersion": MODERN,
                "io.modelcontextprotocol/clientCapabilities": {},
                "io.modelcontextprotocol/clientInfo": {"name": "overlook-milestone-tests", "version": "1.0.0"}
            });
        }
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let line = self
            .output
            .recv_timeout(TIMEOUT)
            .unwrap_or_else(|error| {
                panic!(
                    "{method} ({}): {error}: {}",
                    self.version,
                    self.process.diagnostics()
                )
            })
            .unwrap_or_else(|error| {
                panic!("{method} ({}): bad stdout frame: {error}", self.version)
            });
        let reply: Value = serde_json::from_str(&line).unwrap_or_else(|error| {
            panic!(
                "{method} ({}): invalid JSON-RPC stdout: {error}: {line:?}",
                self.version
            )
        });
        assert_eq!(reply["jsonrpc"], "2.0", "{method}: {reply}");
        assert_eq!(reply["id"], id, "{method}: {reply}");
        assert!(
            reply.get("error").is_none() && reply["result"].is_object(),
            "{method}: {reply}"
        );
        if self.version == MODERN {
            assert_eq!(
                reply["result"]["resultType"], "complete",
                "{method}: {reply}"
            );
        }
        reply["result"].clone()
    }

    fn tool(&mut self, name: &str, arguments: Value) -> Value {
        self.request("tools/call", json!({"name": name, "arguments": arguments}))
    }

    fn read(&mut self, name: &str, arguments: Value) -> Value {
        success(&self.tool(name, arguments)).clone()
    }

    fn accounts(&mut self) -> Vec<Value> {
        success(&self.tool("accounts_list", json!({})))["accounts"]
            .as_array()
            .expect("accounts array")
            .clone()
    }

    fn finish(self) {
        let Self {
            mut process,
            input,
            output,
            ..
        } = self;
        drop(input);
        process.assert_success();
        assert!(
            matches!(
                output.recv_timeout(TIMEOUT),
                Err(RecvTimeoutError::Disconnected)
            ),
            "unexpected stdout after the last response: {}",
            process.diagnostics()
        );
    }
}

fn fixture() -> TempDir {
    // Keep the absolute path short enough for Unix socket names on macOS and Linux.
    tempfile::Builder::new()
        .prefix("ov-mcp-")
        .tempdir_in("/tmp")
        .unwrap()
}

fn command(profile: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_overlook"));
    command
        .args(args)
        .env("OVERLOOK_DATA_DIR", profile)
        .env("OVERLOOK_CONFIG_DIR", profile)
        // Profiles under the same fixture share the runtime directory.
        .env("XDG_RUNTIME_DIR", profile.parent().unwrap())
        .env_remove("OVERLOOK_MCP_TOKEN")
        .env_remove("OVERLOOK_SIGN_IN")
        .env_remove("OVERLOOK_SAMPLE_ONLINE")
        .stdin(Stdio::null())
        .stdout(Stdio::null());
    command
}

fn start_owner(profile: &Path) -> Process {
    let mut process = Process::spawn(command(profile, &["mcp", "sample"]));
    let deadline = Instant::now() + TIMEOUT;
    loop {
        process.assert_running();
        if fs::read_to_string(process.stderr.path())
            .unwrap()
            .lines()
            .any(|line| line == "Overlook MCP sample profile is ready.")
        {
            return process;
        }
        assert!(
            Instant::now() < deadline,
            "sample owner did not become ready: {}",
            process.diagnostics()
        );
        thread::sleep(POLL);
    }
}

fn quit_owner(profile: &Path, owner: &mut Process) {
    Process::spawn(command(profile, &["--quit"])).assert_success();
    owner.assert_success();
}

fn grant(profile: &Path, client: &str, accounts: &[&str]) -> String {
    let mut args = vec!["mcp", "grant", "--client", client];
    for account in accounts {
        args.extend(["--account-id", account]);
    }
    let output = NamedTempFile::new().unwrap();
    let mut command = command(profile, &args);
    command.stdout(output.reopen().unwrap());
    Process::spawn(command).assert_success();
    let stdout = fs::read_to_string(output.path()).unwrap();
    let mut lines = stdout.lines();
    let token = lines.next().expect("grant must print one token");
    assert!(
        !token.is_empty() && !token.chars().any(char::is_whitespace) && lines.next().is_none(),
        "grant stdout must contain only the token"
    );
    token.to_owned()
}

struct Process {
    child: Child,
    args: String,
    stderr: NamedTempFile,
}

impl Process {
    fn spawn(mut command: Command) -> Self {
        let args = format!("{:?}", command.get_args());
        let stderr = NamedTempFile::new().unwrap();
        let child = command
            .stderr(stderr.reopen().unwrap())
            .spawn()
            .unwrap_or_else(|error| panic!("could not start overlook {args}: {error}"));
        Self {
            child,
            args,
            stderr,
        }
    }

    fn diagnostics(&self) -> String {
        let stderr = fs::read_to_string(self.stderr.path()).unwrap_or_default();
        format!("overlook {} stderr: {}", self.args, stderr.trim())
    }

    fn assert_running(&mut self) {
        assert!(
            self.child.try_wait().unwrap().is_none(),
            "unexpected exit: {}",
            self.diagnostics()
        );
    }

    fn wait(&mut self) -> ExitStatus {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {}",
                self.diagnostics()
            );
            thread::sleep(POLL);
        }
    }

    fn assert_success(&mut self) {
        let status = self.wait();
        assert!(status.success(), "{status}: {}", self.diagnostics());
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

use std::fs;
use std::io::Write;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use overlook_core::{Account, AccountId, ProviderKind};
use overlook_store::Store;
use serde_json::{json, Value};

const TIMEOUT: Duration = Duration::from_secs(15);
const READY: &str = "Overlook MCP headless profile is ready.";
const SAMPLE_READY: &str = "Overlook MCP sample profile is ready.";

fn command(profile: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_overlook"));
    command
        .args(args)
        .env("OVERLOOK_DATA_DIR", profile)
        .env("OVERLOOK_CONFIG_DIR", profile)
        .env_remove("OVERLOOK_MCP_TOKEN")
        .env_remove("OVERLOOK_SIGN_IN")
        .env_remove("OVERLOOK_SAMPLE_ONLINE")
        .stdin(Stdio::null());
    command
}

struct Process {
    child: Child,
    stderr: tempfile::NamedTempFile,
}

impl Process {
    fn spawn(mut command: Command) -> Self {
        let stderr = tempfile::NamedTempFile::new().unwrap();
        let child = command.stderr(stderr.reopen().unwrap()).spawn().unwrap();
        Self { child, stderr }
    }

    fn stderr(&self) -> String {
        fs::read_to_string(self.stderr.path()).unwrap()
    }

    fn ready(&mut self, line: &str) -> bool {
        self.stderr().lines().any(|observed| observed == line)
    }

    fn wait(&mut self) -> ExitStatus {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "process timed out: {}",
                self.stderr()
            );
            thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn wait_ready(process: &mut Process, line: &str) {
    let deadline = Instant::now() + TIMEOUT;
    while !process.ready(line) {
        assert!(
            process.child.try_wait().unwrap().is_none() && Instant::now() < deadline,
            "owner failed to start: {}",
            process.stderr()
        );
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn concurrent_headless_startups_share_one_owner_across_profile_aliases() {
    let dir = tempfile::tempdir().unwrap();
    let profile = dir.path().join("profile");
    fs::create_dir(&profile).unwrap();
    let alias = dir.path().join("alias");
    symlink(&profile, &alias).unwrap();
    let mut processes: Vec<_> = [profile.as_path(), &alias]
        .into_iter()
        .map(|path| {
            let mut command = command(path, &["mcp", "headless"]);
            command.env("XDG_RUNTIME_DIR", path);
            Process::spawn(command)
        })
        .collect();
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let ready = processes
            .iter_mut()
            .filter_map(|p| p.ready(READY).then_some(()))
            .count();
        let refused = processes
            .iter_mut()
            .filter_map(|p| p.child.try_wait().unwrap())
            .filter(|status| !status.success())
            .count();
        if ready == 1 && refused == 1 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "headless startup failed to select one owner"
        );
        thread::sleep(Duration::from_millis(20));
    }
    assert!(profile.join("overlook.sqlite").is_file());
    assert!(!profile.join(".mcp/sample.sqlite").exists());

    // Autostart leaves the active headless owner intact.
    assert!(Process::spawn(command(&profile, &["--background"]))
        .wait()
        .success());
    assert!(Process::spawn(command(&alias, &["--quit"]))
        .wait()
        .success());
    let active = processes
        .iter_mut()
        .find(|p| p.stderr().contains(READY))
        .unwrap();
    assert!(active.wait().success(), "{}", active.stderr());
}

#[test]
fn sample_and_desktop_databases_stay_separate_after_shutdown() {
    let dir = tempfile::tempdir().unwrap();
    let profile = dir.path().join("profile");
    let mut sample = Process::spawn(command(&profile, &["mcp", "sample", "--offline"]));
    wait_ready(&mut sample, SAMPLE_READY);
    let mut desktop = Process::spawn(command(&profile, &[]));
    assert!(!desktop.wait().success());
    assert!(desktop
        .stderr()
        .contains("reserved for the MCP sample profile"));
    assert!(sample.child.try_wait().unwrap().is_none());
    assert!(Process::spawn(command(&profile, &["--quit"]))
        .wait()
        .success());
    assert!(sample.wait().success());

    for args in [&["mcp", "headless"][..], &[]] {
        let mut process = Process::spawn(command(&profile, args));
        assert!(!process.wait().success());
        assert!(process
            .stderr()
            .contains("reserved for the MCP sample profile"));
    }
    assert!(profile.join(".mcp/sample.sqlite").is_file());
    assert!(!profile.join("overlook.sqlite").exists());
}

#[test]
fn signals_preserve_cached_accounts_and_opaque_ids_in_the_real_profile() {
    let dir = tempfile::tempdir().unwrap();
    let profile = dir.path().join("profile");
    let database = profile.join("overlook.sqlite");
    {
        let store = Store::open(&database).unwrap();
        store
            .upsert_account(&Account {
                id: AccountId::from("sample"),
                provider: ProviderKind::Sample,
                display_name: "Cached Example".into(),
                email: "fictional@example.com".into(),
                created_at: 0,
            })
            .unwrap();
    }

    let grant = command(
        &profile,
        &["mcp", "grant", "--client", "test", "--account-id", "sample"],
    )
    .output()
    .unwrap();
    assert!(grant.status.success());
    let token = String::from_utf8(grant.stdout).unwrap();
    let mut previous = None;
    for signal in [libc::SIGINT, libc::SIGTERM] {
        let mut owner = Process::spawn(command(&profile, &["mcp", "headless"]));
        wait_ready(&mut owner, READY);
        let account = account_via_stdio(&profile, token.trim());
        assert_eq!(account["display_name"], "Cached Example");
        assert!(account["account_id"].is_string());
        if let Some(old) = previous.replace(account) {
            assert_eq!(previous.as_ref(), Some(&old));
        }

        let mut sample = Process::spawn(command(&profile, &["mcp", "sample", "--offline"]));
        assert!(!sample.wait().success());
        // SAFETY: owner is the isolated, still-running child spawned above.
        assert_eq!(
            unsafe { libc::kill(owner.child.id() as libc::pid_t, signal) },
            0
        );
        assert!(owner.wait().success(), "{}", owner.stderr());
    }
    let mut sample = Process::spawn(command(&profile, &["mcp", "sample", "--offline"]));
    assert!(!sample.wait().success());
    assert!(sample.stderr().contains("without an Overlook database"));
    assert!(!profile.join(".mcp/sample.sqlite").exists());
}

fn account_via_stdio(profile: &Path, token: &str) -> Value {
    let output = tempfile::NamedTempFile::new().unwrap();
    let mut bridge = command(profile, &["mcp", "serve", "--client", "test"]);
    bridge
        .env("OVERLOOK_MCP_TOKEN", token)
        .stdin(Stdio::piped())
        .stdout(output.reopen().unwrap());
    let mut bridge = Process::spawn(bridge);
    {
        let mut input = bridge.child.stdin.take().unwrap();
        let call = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "accounts_list",
                "arguments": {},
                "_meta": {
                    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                    "io.modelcontextprotocol/clientCapabilities": {},
                },
            },
        });
        writeln!(input, "{call}").unwrap();
    }
    assert!(bridge.wait().success(), "{}", bridge.stderr());
    let reply: Value = serde_json::from_reader(output.reopen().unwrap()).unwrap();
    let account = &reply["result"]["structuredContent"]["accounts"][0];
    assert!(
        account.is_object(),
        "unexpected accounts_list reply: {reply}"
    );
    account.clone()
}

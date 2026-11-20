//! Owns the MCP endpoint in the desktop process or the synthetic sample process.

mod contract;
mod read;
mod service;
mod transport;
mod write;

use anyhow::{bail, ensure, Context, Result};
use overlook_core::{time::now, Account, AccountId, Prefs, ProviderKind, SyncState};
use overlook_provider::sample::SampleProvider;
use overlook_store::Store;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, DirBuilder};
use std::io::{self, Write};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::{
    data_dir, default_data_dir, seed_sample_account, seed_sample_mailbox, single_instance,
};
use write::WritePermission;

static DESKTOP_SERVER: Mutex<Option<service::Server>> = Mutex::new(None);
static MAIL_CHANGES: Mutex<BTreeMap<AccountId, bool>> = Mutex::new(BTreeMap::new());

fn note_write(account: &AccountId, queued: bool) {
    *MAIL_CHANGES
        .lock()
        .unwrap()
        .entry(account.clone())
        .or_default() |= queued;
    single_instance::wake();
}

pub(crate) fn take_mail_changes() -> BTreeMap<AccountId, bool> {
    std::mem::take(&mut *MAIL_CHANGES.lock().unwrap())
}

const HELP: &str = r#"Usage: overlook mcp <command>

  sample [--offline]                   run a foreground owner with two fictional accounts
  grant --client NAME --account-id ID   issue or replace a grant (repeat --account-id to allow more)
          [--allow modify|send|permanent-delete]   add a write scope (repeat to combine)
  revoke --client NAME                  revoke a grant
  serve --client NAME                  bridge an MCP client over stdio to the running owner

  OVERLOOK_DATA_DIR    selects the profile. Sample mode requires a separate directory.
  OVERLOOK_MCP_TOKEN   credential supplied by the MCP client to the serve command.

Use sample or sample-secondary for the fictional accounts. For eligible IMAP and POP3
accounts, use imap:<address> or pop3:<address> as stored by Overlook."#;

pub fn start_desktop(profile: &Path, store: Arc<Store>, sample: SampleProvider) {
    match service::start(profile, store, sample) {
        Ok(server) => *DESKTOP_SERVER.lock().unwrap() = Some(server),
        Err(error) => tracing::warn!("the local MCP endpoint could not start: {error:#}"),
    }
}

pub fn stop_desktop() {
    drop(DESKTOP_SERVER.lock().unwrap().take());
}

pub fn run(args: &[String]) -> Result<()> {
    let Some((command, flags)) = args.split_first() else {
        println!("{HELP}");
        return Ok(());
    };
    match command.as_str() {
        "--help" | "-h" if flags.is_empty() => {
            println!("{HELP}");
            Ok(())
        }
        "sample" if flags.is_empty() => run_sample(true),
        "sample" if flags == ["--offline"] => run_sample(false),
        "serve" => {
            let (client, _, _) = client_flags(flags, false)?;
            let profile = data_dir();
            let token = std::env::var("OVERLOOK_MCP_TOKEN").unwrap_or_default();
            transport::serve(move |request| service::call(&profile, &client, &token, request))
        }
        "grant" => {
            let (client, accounts, permissions) = client_flags(flags, true)?;
            ensure!(
                !accounts.is_empty(),
                "grant requires at least one --account-id"
            );
            let token = service::grant(&data_dir(), &client, &accounts, permissions)?;
            writeln!(io::stdout().lock(), "{token}")
                .context("could not return the client credential")
        }
        "revoke" => {
            let (client, _, _) = client_flags(flags, false)?;
            service::revoke(&data_dir(), &client)
        }
        _ => bail!("invalid command or arguments. Run overlook mcp --help."),
    }
}

fn client_flags(
    flags: &[String],
    grant: bool,
) -> Result<(String, Vec<String>, BTreeSet<WritePermission>)> {
    let mut client = None;
    let mut accounts = Vec::new();
    let mut permissions = BTreeSet::new();
    let mut flags = flags.iter();
    while let Some(flag) = flags.next() {
        let value = flags
            .next()
            .filter(|value| !value.is_empty() && !value.starts_with('-'))
            .context("each MCP option needs a value")?;
        match flag.as_str() {
            "--client" if client.is_none() => client = Some(value.clone()),
            "--account-id" if grant => accounts.push(value.clone()),
            "--allow" if grant => {
                let permission = match value.as_str() {
                    "modify" => WritePermission::Modify,
                    "send" => WritePermission::Send,
                    "permanent-delete" => WritePermission::PermanentDelete,
                    _ => bail!("Unknown write scope. Run overlook mcp --help."),
                };
                permissions.insert(permission);
            }
            _ => bail!("invalid or repeated option. Run overlook mcp --help."),
        }
    }
    Ok((
        client.context("--client is required")?,
        accounts,
        permissions,
    ))
}

fn run_sample(online: bool) -> Result<()> {
    let profile = sample_profile()?;
    ensure!(
        single_instance::listen().context("could not claim the sample profile")?,
        "Overlook is already running for this profile"
    );
    let result = serve_sample(&profile, online);
    single_instance::release();
    result
}

fn sample_profile() -> Result<PathBuf> {
    let profile = data_dir();
    ensure!(
        std::env::var_os("OVERLOOK_DATA_DIR").is_some_and(|value| !value.is_empty()),
        "sample mode requires a separate OVERLOOK_DATA_DIR"
    );
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&profile)?;
    let profile = profile.canonicalize()?;
    ensure!(
        default_data_dir().canonicalize().ok().as_ref() != Some(&profile),
        "sample mode requires a directory outside the default profile"
    );
    match fs::symlink_metadata(profile.join("overlook.sqlite")) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(profile),
        Ok(_) => bail!("sample mode requires a separate directory without an Overlook database"),
        Err(error) => Err(error).context("could not inspect the sample directory"),
    }
}

fn serve_sample(profile: &Path, online: bool) -> Result<()> {
    let store = Arc::new(Store::open(&service::sample_database(profile)?)?);
    let initialized = store
        .setting::<bool>("mcp_sample_initialized")?
        .unwrap_or(false);
    if !initialized {
        let primary = seed_sample_account(&store)?;
        let secondary = Account {
            id: AccountId::from("sample-secondary"),
            provider: ProviderKind::Sample,
            display_name: "Sample Secondary".into(),
            email: "secondary@example.com".into(),
            created_at: now(),
        };
        seed_sample_mailbox(&store, &secondary)?;
        read::seed_sample(&store, &primary)?;
        read::seed_sample(&store, &secondary)?;
        store.set_sync_state(
            &primary.id,
            &SyncState::Offline,
            Some(now() - 2 * 86_400_000),
        )?;
        store.set_setting(
            "prefs",
            &Prefs {
                undo_send_secs: 0,
                ..Prefs::default()
            },
        )?;
        store.set_setting("mcp_sample_initialized", &true)?;
    }
    let sample = SampleProvider::new().with_latency(std::time::Duration::ZERO);
    let sample = if initialized {
        sample.with_incomplete_send_history()
    } else {
        sample
    };
    sample.set_online(online);
    let _server = service::start(profile, store.clone(), sample.clone())?;
    if online && initialized {
        replay_sample(&store, &sample)?;
    }
    eprintln!("Overlook MCP sample profile is ready.");
    let mut wakeups = single_instance::wakeups();
    loop {
        if single_instance::take().iter().any(|line| {
            matches!(
                single_instance::Request::parse(line),
                Some(single_instance::Request::Quit)
            )
        }) {
            return Ok(());
        }
        let changed = take_mail_changes();
        if online && changed.values().any(|queued| *queued) {
            replay_sample(&store, &sample)?;
        }
        if wakeups.blocking_recv().is_none() {
            return Ok(());
        }
    }
}

fn replay_sample(store: &Arc<Store>, sample: &SampleProvider) -> Result<()> {
    let accounts = store.accounts()?;
    for id in store.mcp_accounts_with_pending_writes()? {
        if let Some(account) = accounts
            .iter()
            .find(|a| a.id == id && a.provider == ProviderKind::Sample)
        {
            crate::sync::pollster_block(crate::sync::replay_sample_mail(store, sample, account));
        }
    }
    Ok(())
}

use super::{
    APPLICATION, KIND_OAUTH_MICROSOFT, KIND_OAUTH_MICROSOFT_CONTACTS, KIND_OAUTH_TOKENS,
    KIND_PASSWORD_INCOMING, KIND_PASSWORD_OUTGOING,
};
use overlook_core::AccountId;
use std::collections::HashMap;
use std::sync::OnceLock;

/// True only when this process really runs inside a Flatpak or Snap sandbox, where the
/// secrets portal is the way to the keyring. The same test `oo7` makes through `ashpd`
/// (`/.flatpak-info`, or the process's own cgroup for a Snap) without its last clause.
/// Environment variables such as `FLATPAK_ID` or `SNAP` aren't read: a program started from a
/// Flatpak or Snap app's terminal inherits them without being sandboxed itself.
///
/// `GTK_USE_PORTAL` is deliberately not a signal. It asks GTK to send GTK's own dialogs
/// through the xdg-desktop-portal; it says nothing about the process being sandboxed. The
/// `oo7` 0.4 helper behind `Keyring::new` treats it as proof of a sandbox, so a user who set
/// it session-wide had mail passwords written to `~/.local/share/keyrings/default.keyring`
/// instead of the system keyring. That file is encrypted with the secrets portal's
/// per-application secret, and the portal derives the application id from how the app was
/// launched: once an `ai.lastrefuge.Overlook.desktop` entry exists, KDE starts the app as
/// `app-ai.lastrefuge.Overlook@<id>.service`, the portal hands it a different secret, and
/// every stored password then reads back as `IncorrectSecret`.
fn sandboxed() -> bool {
    let cgroup = std::fs::read_to_string("/proc/self/cgroup").unwrap_or_default();
    sandboxed_with(std::path::Path::new("/"), &cgroup)
}

/// [`sandboxed`] with its inputs passed in, so the rule can be checked without a container.
fn sandboxed_with(root: &std::path::Path, cgroup: &str) -> bool {
    root.join(".flatpak-info").exists() || in_snap(cgroup)
}

/// Whether a `/proc/self/cgroup` listing puts the process in a Snap's scope
/// (`snap.<name>.<app>…`), as `ashpd` checks.
fn in_snap(cgroup: &str) -> bool {
    cgroup.lines().any(|line| {
        let Some((number, rest)) = line.split_once(':') else {
            return false;
        };
        let unit = match rest.split_once(':') {
            Some(("" | "freezer" | "name=systemd", unit)) if number.parse::<u32>().is_ok() => unit,
            _ => return false,
        };
        std::path::Path::new(unit)
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|scope| scope.starts_with("snap."))
    })
}

/// The keyring to use: the Secret Service outside a sandbox, the portal's file backend inside
/// one. Chosen here rather than by `oo7::Keyring::new`, which asks `ashpd` whether the process
/// is sandboxed and counts `GTK_USE_PORTAL` as proof. Like `oo7`, a sandbox without the
/// secrets portal uses the Secret Service, which the Flatpak is allowed to reach.
async fn keyring() -> Result<oo7::Keyring, oo7::Error> {
    if sandboxed() {
        match oo7::file::Keyring::load_default().await {
            Ok(file) => {
                tracing::debug!(target: "overlook::diag", "sandboxed: using the secrets portal file keyring");
                return Ok(oo7::Keyring::File(std::sync::Arc::new(file)));
            }
            Err(oo7::file::Error::Portal(oo7::ashpd::Error::PortalNotFound(_))) => {
                tracing::debug!(target: "overlook::diag", "sandboxed without the secrets portal: using the Secret Service");
            }
            Err(e) => return Err(oo7::Error::File(e)),
        }
    }
    let service = oo7::dbus::Service::<'static>::new().await?;
    Ok(oo7::Keyring::DBus(service.default_collection().await?))
}

fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .thread_name("overlook-keyring")
            .build()
            .expect("tokio runtime")
    })
}

fn attributes(account: &AccountId, kind: &str) -> HashMap<&'static str, String> {
    HashMap::from([
        ("application", APPLICATION.to_owned()),
        ("account", account.0.clone()),
        ("kind", kind.to_owned()),
    ])
}

/// Attributes shared by every secret of an account, for deleting them all at once.
fn account_attributes(account: &AccountId) -> HashMap<&'static str, String> {
    HashMap::from([
        ("application", APPLICATION.to_owned()),
        ("account", account.0.clone()),
    ])
}

fn borrow<'a>(attrs: &'a HashMap<&'static str, String>) -> HashMap<&'a str, &'a str> {
    attrs.iter().map(|(k, v)| (*k, v.as_str())).collect()
}

/// The item label shown in keyring managers.
fn label(account: &AccountId, kind: &str) -> String {
    let what = match kind {
        KIND_OAUTH_TOKENS => "Gmail sign-in",
        KIND_OAUTH_MICROSOFT => "Microsoft sign-in",
        KIND_OAUTH_MICROSOFT_CONTACTS => "Microsoft contacts sign-in",
        KIND_PASSWORD_INCOMING => "mail server password",
        KIND_PASSWORD_OUTGOING => "outgoing server password",
        overlook_core::ContactsConnection::SECRET_KIND => "contacts server password",
        other => other,
    };
    format!("Overlook {what} ({})", account.0)
}

pub(super) fn save(account: &AccountId, kind: &str, secret: Vec<u8>) -> Result<(), oo7::Error> {
    let attrs = attributes(account, kind);
    let label = label(account, kind);
    runtime().block_on(async move {
        keyring()
            .await?
            .create_item(&label, &borrow(&attrs), secret, true)
            .await
    })
}

pub(super) fn load(account: &AccountId, kind: &str) -> Result<Option<Vec<u8>>, oo7::Error> {
    let attrs = attributes(account, kind);
    runtime().block_on(async move {
        let keyring = keyring().await?;
        let items = keyring.search_items(&borrow(&attrs)).await?;
        let Some(item) = items.into_iter().next() else {
            return Ok(None);
        };
        Ok(Some(item.secret().await?.as_bytes().to_vec()))
    })
}

pub(super) fn delete(account: &AccountId, kind: &str) -> Result<(), oo7::Error> {
    let attrs = attributes(account, kind);
    runtime().block_on(async move { keyring().await?.delete(&borrow(&attrs)).await })
}

pub(super) fn delete_all(account: &AccountId) -> Result<(), oo7::Error> {
    let attrs = account_attributes(account);
    runtime().block_on(async move { keyring().await?.delete(&borrow(&attrs)).await })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn microsoft_sign_in_label() {
        assert_eq!(
            label(
                &AccountId("imap:j@outlook.com".into()),
                KIND_OAUTH_MICROSOFT
            ),
            "Overlook Microsoft sign-in (imap:j@outlook.com)"
        );
    }

    #[test]
    fn a_portal_hint_is_not_a_sandbox() {
        // GTK_USE_PORTAL=1, set session-wide, used to send every mail password to oo7's file
        // keyring: its key is the portal's per-application secret, and the portal changes the
        // application id once a .desktop entry lets it tell how the app was launched. The
        // decision reads no environment variable at all, so neither that hint nor a SNAP or
        // FLATPAK_ID inherited from a sandboxed app's terminal can move the passwords.
        let bare = std::path::Path::new("/nonexistent-overlook-sandbox-root");
        let desktop = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-ai.lastrefuge.Overlook@x.service\n";
        assert!(!sandboxed_with(bare, desktop));
        assert!(!sandboxed_with(bare, ""));
        assert!(sandboxed_with(bare, "0::/user.slice/user-1000.slice/user@1000.service/app.slice/snap.overlook.overlook-4b2c.scope\n"));
        assert!(sandboxed_with(
            bare,
            "12:freezer:/snap.overlook.overlook\n0::/user.slice\n"
        ));
        assert!(
            !sandboxed_with(
                bare,
                "0::/user.slice/user-1000.slice/user@1000.service/app.slice/snap-code.scope\n"
            ),
            "not a snap's own scope"
        );

        let container = tempfile::tempdir().unwrap();
        std::fs::write(container.path().join(".flatpak-info"), b"[Application]\n").unwrap();
        assert!(sandboxed_with(container.path(), desktop));
    }
}

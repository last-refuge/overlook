# Secrets access policy

This policy covers Overlook 1.6.1. It lists every secret Overlook keeps, which parts of Overlook may read,
write or delete it, which users and processes the operating system lets reach it, and how that access
is logged. All statements are checked against the source; paths are relative to the repository root.

## Where secrets live

Overlook keeps secrets only in the system keyring, through the `oo7` crate (0.4). Outside a sandbox,
`oo7` talks to the freedesktop Secret Service (GNOME Keyring, KWallet) and uses its default collection.
If the keyring is missing or locked, Overlook shows an error and stops; it never falls back to a file
(`crates/overlook-app/src/credentials.rs:1-5`).

Every secret is one keyring item with exactly three attributes (`credentials.rs:99-101`):

| Attribute     | Value                                                                     |
|---------------|---------------------------------------------------------------------------|
| `application` | `ai.lastrefuge.Overlook` (`credentials.rs:22`)                            |
| `account`     | the Overlook account id: `gmail:<address>`, `imap:<address>` or `pop3:<address>` |
| `kind`        | one of the values in the table below                                      |

The item label shown in keyring managers is `Overlook <what> (<account id>)` (`credentials.rs:113-121`).
Because the account id contains the mail address, the address appears in the label and the `account`
attribute. The secret value never does.

## Secrets

| Secret | `kind` | Contents | Written by | Read by | Deleted by |
|---|---|---|---|---|---|
| Google OAuth tokens | `oauth-tokens` | JSON of `OAuthTokens`: `access_token`, `refresh_token`, `expires_at`, `scope`, `signed_in_at` (`crates/overlook-provider/src/gmail/oauth.rs:35-46`) | Google sign-in completing (`crates/overlook-app/src/ui/mod.rs:1227`); each access-token refresh, through the client's token sink (`crates/overlook-app/src/providers.rs:49`) | Provider creation when an account loads (`providers.rs:110`); sign-out or removal when no live client exists, to revoke the grant first (`ui/mod.rs:314`) | Google rejecting the refresh token with `invalid_grant` (`providers.rs:56`); sign-out, removal, and automatic sign-out after 90 days idle or 180 days since sign-in (`ui/mod.rs:310-322`, `ui/mod.rs:981-986`) |
| IMAP or POP3 password | `password:incoming` | UTF-8 password | Account setup and "Update password" (`crates/overlook-app/src/ui/account_setup.rs:528`) | Provider creation (`providers.rs:90`); setup's connection test and form (`account_setup.rs:420`); CalDAV sync when the calendar reuses the mail password (`crates/overlook-app/src/caldav_sync.rs:113-118`) | Sign-out and removal (`ui/mod.rs:319`) |
| SMTP password | `password:outgoing` | UTF-8 password; absent when "same as incoming" is chosen | Account setup (`account_setup.rs:533`) | Provider creation (`providers.rs:92`); setup form (`account_setup.rs:166`, `:428`) | Setup when "same as incoming" is chosen (`account_setup.rs:534`); sign-out and removal (`ui/mod.rs:319`) |
| CalDAV app password (for example Fastmail) | `password:calendar:<connection id>` (`crates/overlook-core/src/calendar_connection.rs:76-78`) | UTF-8 password; only when the connection uses a separate credential | Calendar connection setup and password change (`crates/overlook-app/src/ui/calendar_setup.rs:286`, `:336`) | CalDAV sync (`caldav_sync.rs:118`) | Setup failure or superseded setup (`calendar_setup.rs:288`, `:339`); replacing the connection (`:295`); disconnecting calendars (`:381`); sign-out and removal (`ui/mod.rs:319`) |

Lifetime: each item exists from the write until one of the deletions listed. Google OAuth tokens also
end at Google's side: sign-out and removal revoke the grant before the local delete, and a session is
signed out automatically after 90 days without a refresh (`SESSION_IDLE_LIMIT_MS`) or 180 days after
sign-in (`SESSION_MAX_AGE_MS`), see `crates/overlook-provider/src/gmail/api.rs:45-51` and `:448-456`.
Passwords have no expiry inside Overlook; they last until sign-out, removal or replacement.

### The Google OAuth client file

`google-oauth-client.json` is not stored in the keyring. The user downloads it from their own Google
Cloud project and saves it in `~/.config/overlook/` (or `$OVERLOOK_CONFIG_DIR`); see
`crates/overlook-app/src/google_signin.rs:19-28`. Overlook only reads it (`google_signin.rs:31-40`), at
sign-in and when it creates a Gmail client (`providers.rs:41`); it never writes, copies or deletes it.
It holds the client id and the "Desktop app" client secret, which Google documents as
non-confidential for installed apps; PKCE protects the authorization code. Its `Debug` output redacts
the secret (`oauth.rs:48-52`). File permissions are whatever the user saved it with; owner-only
(`chmod 600`) is recommended.

## Who can access the keyring items

| Party | Access |
|---|---|
| The desktop user who owns the session, while the keyring is unlocked | All items, through Overlook or any keyring manager (Seahorse, KWalletManager, `secret-tool`). |
| Other processes in that user's session | Can query the Secret Service over the session D-Bus. This is the documented Secret Service model: the keyring trusts the unlocked user session, not individual programs. |
| Other local users, and the user's processes when the keyring is locked | No access. Items are encrypted at rest by the keyring daemon and the session bus is per user. |
| Last Refuge or any remote server | No access. Overlook has no server of its own; tokens go only to Google, passwords only to the user's configured mail and CalDAV servers. |

Released builds (Arch package and tarball) run unsandboxed and use the Secret Service as above.
A Flatpak manifest exists as a draft (`packaging/ai.lastrefuge.Overlook.yml`) but is not built or
shipped. In a sandbox `oo7` would use its file backend, encrypted with a key obtained from the Secret
portal, so only Overlook could open its own items; the draft also allows `org.freedesktop.secrets`,
which `oo7` uses when the portal is missing. That path will be documented when a Flatpak ships.

## Least privilege in the code

All keyring access goes through one module, `crates/overlook-app/src/credentials.rs`. It is the only
Overlook code that calls `oo7` (declared only in `crates/overlook-app/Cargo.toml`), and every other
module reaches the keyring through it. Its public functions:

| Function | Line | Scope |
|---|---|---|
| `save_secret(account, kind, secret)` | 124 | one item: exact `application`, `account`, `kind` |
| `load_secret(account, kind)` | 136 | one item: exact three attributes |
| `delete_secret(account, kind)` | 151 | one item: exact three attributes |
| `delete_all_secrets(account)` | 162 | every item of one account: `application` and `account`, any `kind` |
| `save_tokens` / `load_tokens` / `delete_tokens` | 175, 181, 189 | wrappers over the above with `kind = oauth-tokens` |
| `save_password` / `load_password` | 195, 201 | wrappers with a password `kind` |

There is no wildcard search across accounts or applications. The only broader query is
`delete_all_secrets`, used by sign-out and account removal (`ui/mod.rs:319`): it matches on
`application` and `account` together, so it removes every kind for that one account, including CalDAV
passwords, and nothing else.

## Never stored elsewhere

| Place | Mechanism |
|---|---|
| Configuration and SQLite | Server settings types exclude passwords by design (`crates/overlook-core/src/servers.rs:84`, `:181`); the store notes passwords are kept in the keyring only (`crates/overlook-store/src/lib.rs:1030`). No code path writes tokens or passwords to a file. |
| `Debug` output | Redacting `Debug` impls: `OAuthTokens` and `ClientConfig` (`oauth.rs:48-58`), PKCE (`oauth.rs:257`), `ImapProvider` (`crates/overlook-provider/src/imap/mod.rs:425`), `Pop3Provider` (`crates/overlook-provider/src/pop3/mod.rs:177`), SMTP `Transport` (`crates/overlook-provider/src/smtp.rs:36`), `CalDavClient` (`crates/overlook-provider/src/caldav/mod.rs:113-119`). |
| Logs | Release builds use the fixed filter `overlook=info` and ignore `RUST_LOG` (`crates/overlook-app/src/main.rs:138-146`). Library targets that can carry wire bytes or secrets (`ureq`, `rustls`, `lettre`, `imap`, `oo7`, `zbus`, `zvariant`) are dropped in every build (`main.rs:166-168`). Keyring library errors go only to `overlook::diag` at debug level, which release builds do not record, and users see constant messages (`credentials.rs:77-85`). The log file is mode 0600 in a 0700 data folder (`main.rs:118-132`). |
| Crash output | Release builds replace the panic output with the fixed line `PANIC_NOTICE` on stderr and send the location and message only to the private log (`main.rs:150-162`). |

## Access logging

Every keyring read, write and delete writes one line under the `overlook::vault_audit` target at info
level (`credentials.rs:72-74`, called through `audited` at `credentials.rs:88-97`). Fields:

- `op`: `read`, `write`, `delete` or `delete-all`
- `kind`: the item kind, or `*` for `delete-all`
- `account`: the account id
- `outcome`: `ok`, `not-found` (read only), `keyring-unavailable` or `corrupt`
- timestamp, added by the log formatter

The secret value is never passed to the audit function. Lines go to stderr and to `overlook.log` in
the data folder (`~/.local/share/overlook/`), which the release filter keeps. Example:

```
2026-10-28T09:14:02.511Z  INFO overlook::vault_audit: keyring access op="read" kind="oauth-tokens" account=gmail:user@example.com outcome="ok"
```

Test: `credentials::tests::keyring_access_is_audited_without_secret_material`
(`credentials.rs:244-257`) feeds a synthetic secret through the audit path and checks that the line
has the expected fields and does not contain the secret. Result on v1.6.1:

```
$ cargo test -p overlook --bin overlook credentials::
test credentials::tests::user_facing_vault_errors_are_constant_and_detail_stays_in_debug_log ... ok
test credentials::tests::keyring_access_is_audited_without_secret_material ... ok
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 102 filtered out; finished in 0.00s
```

## Monitoring

The user can watch keyring access from outside Overlook while it runs:

```
dbus-monitor --session "interface='org.freedesktop.Secret.Service'" "interface='org.freedesktop.Secret.Item'"
```

This shows each `SearchItems`, `GetSecrets`, `CreateItem` and `Delete` call with the calling process's
bus name. Each call should match an `overlook::vault_audit` line in `overlook.log` with the same time,
operation and kind. Calls with no matching audit line come from another program in the session.

A dated capture of both, taken during sign-in, sync and sign-out with a test account, is attached
separately to the CASA submission.

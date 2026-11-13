# Microsoft accounts over IMAP, POP3 and SMTP

Written 12 November 2026 for release target 1.6.22. This is milestone 1 of priority 1 in the [mail service provider roadmap](mail-provider-roadmap.md): Outlook.com, Hotmail, Live and MSN addresses, and Microsoft 365 work or school accounts, signed in with Microsoft instead of a password.

The user chose this architecture and the client-id model on 12 November 2026:

- **IMAP/SMTP with OAuth, not Microsoft Graph.** The existing IMAP, POP3 and SMTP providers are used as they are; only how they sign in changed (SASL `XOAUTH2`). Folders, IDLE, two-way draft sync, the Outbox, POP3 retention and search behave as for any IMAP or POP3 account. The same plumbing is meant to serve Yahoo and AOL sign-in later (roadmap priority 3). A Graph provider remains the route to Outlook calendar and contacts.
- **A built-in Last Refuge app registration, with an override.** Microsoft desktop clients ship a public client id (Thunderbird and Evolution do). A file can replace it for development or a packager's own registration.

Microsoft disabled password (basic) authentication for Outlook.com IMAP, POP3 and SMTP in September 2024, and Microsoft 365 is retiring it for SMTP AUTH. Before this milestone, Overlook had no working way to reach these mailboxes.

## What exists

| Area | Where | What it does |
|---|---|---|
| Server model | `crates/overlook-core/src/servers.rs` | `OAuthIssuer::Microsoft` and `MailServers.oauth`. A Microsoft account is an ordinary `imap:` or `pop3:` account whose servers say Microsoft; both endpoints carry `AuthMethod::OAuth2`. Password accounts store the same JSON as before (the field is omitted). |
| Shared OAuth | `crates/overlook-provider/src/oauth.rs` | Tokens, PKCE, loopback query validation, session limits and the `XOAUTH2` response. Moved out of `gmail::oauth` and `gmail::api`, which re-export them; Google's behaviour is unchanged and its tests still pass. |
| Credential | `crates/overlook-provider/src/credential.rs` | `Credential::Password` or `Credential::OAuth(token source)`. A refused access token is retried once on a fresh connection with a refreshed token; a refused password is never retried. |
| Microsoft sign-in | `crates/overlook-provider/src/microsoft/` | v2.0 authorization URL (PKCE S256, `response_mode=query`, `login_hint`), code exchange and refresh, ID-token claims, `MicrosoftSession`, consumer domains and server settings. |
| Protocols | `imap/connection.rs`, `imap/idle.rs`, `pop3/client.rs`, `smtp.rs` | `AUTHENTICATE XOAUTH2`, POP3 `AUTH XOAUTH2` (the two-step exchange Microsoft documents, cancelling an error challenge with `*`), SMTP `AUTH XOAUTH2` through lettre. |
| Loopback listener | `crates/overlook-app/src/loopback.rs` | Extracted from `google_signin.rs` and shared. It accepts only `GET <path>?…` with one matching `Host` and the request's `state`. Google keeps `127.0.0.1:PORT/callback`; Microsoft uses `localhost:PORT/` and also listens on the same port of `::1`, which browsers try first, so no other program can take the redirect there. |
| App sign-in | `crates/overlook-app/src/microsoft_signin.rs` | Registration loading (file, then built-in), the browser flow, and the code exchange. A declined consent shows a fixed message that mentions administrator approval; the redirect's own error text is never shown. |
| Keyring | `crates/overlook-app/src/credentials.rs` | Kind `oauth-tokens:microsoft`, label "Overlook Microsoft sign-in (…)". The JSON holds the tokens plus the client id and tenant that issued them, so refreshes keep using that registration even if the built-in one or the file changes. |
| Providers | `crates/overlook-app/src/providers.rs` | Builds the session from stored tokens: refreshed tokens are persisted, `invalid_grant` deletes the stored copy. Only an account's newest sign-in may write to the keyring (`adopt_microsoft_sign_in`, `end_microsoft_sign_ins`). A session replaced by a new sign-in, or whose account was signed out, can finish a refresh but never stores it or deletes its successor. Rebuilt providers reuse the running session. `AccountProviders.microsoft` holds the session for sign-out and session limits. |
| UI | `ui/account_setup.rs`, `ui/mod.rs`, `ui/settings.rs`, `ui/sidebar.rs` | "Add Microsoft account…" in the sidebar account menu, Settings → Accounts → Add account, and the empty window; `OVERLOOK_SIGN_IN=microsoft`. The setup dialog's Microsoft mode, Sign in again, Sign out, account labels and the About page's registration line. |

### Signing in

1. The setup dialog in Microsoft mode asks for the account name, your name and the address, with no password. **Sign in with Microsoft** opens the browser at `login.microsoftonline.com/<tenant>/oauth2/v2.0/authorize` with the address as `login_hint`. Clicking again while waiting starts over in a new tab, and closing the dialog cancels the wait.
2. The browser returns to `http://localhost:<port>/?code=…&state=…`. The code is exchanged together with the PKCE verifier, and nothing is stored yet.
3. The ID token's tenant (`tid`) decides the account type; the address domain decides only when there is no token. Microsoft's documented settings fill the Servers step:

   | | Personal (Outlook.com) | Work or school (Microsoft 365) |
   |---|---|---|
   | IMAP | `outlook.office365.com:993`, TLS | same |
   | POP3 | `outlook.office365.com:995`, TLS | same |
   | SMTP | `smtp-mail.outlook.com:587`, STARTTLS | `smtp.office365.com:587`, STARTTLS |

4. Both servers are tested with the new tokens, then the account is saved, as **Add account** does for password accounts. A failed test shows one line per server and offers **Add anyway**.
   - If the ID token names someone other than the address and user name (`IdClaims::names_any`, which checks both `email` and `preferred_username`), nothing is saved by itself. The dialog says who signed in and waits for **Add account**. That covers an alias or a shared mailbox, and catches the wrong account.
5. Saving writes the account, servers and the keyring item, then starts a session that persists its own refreshes.

The dialog switches to Microsoft mode by itself in three cases:

- An Outlook.com, Hotmail, Live or MSN address entered with **Continue** or **Set up servers manually** on the IMAP or POP3 dialog.
- **Update password** on an existing password account at such an address that reads mail from Microsoft's servers. This converts the account in place and keeps its mail. **Server settings…** on such an account stays editable and only explains the switch.
- **Server settings…** on a Microsoft account. There, **Sign in again** keeps the settings as they are (a custom user name or host may be deliberate) and waits for **Save**.

POP3 is reached through **Add POP3 account…** with a Microsoft address; **Add Microsoft account…** sets up IMAP.

**Sign in again** from the banner, sidebar or Settings uses the browser alone. A sign-in that names the account is stored at once. A sign-in as someone else is first used to open the account's mailbox. It is stored only if that works, as it does for an alias or a delegate; otherwise nothing changes and the user is asked to choose the right account.

### Scopes

`https://outlook.office.com/IMAP.AccessAsUser.All` (or `POP.AccessAsUser.All` for POP3 accounts) and `https://outlook.office.com/SMTP.Send`, plus `offline_access openid profile email`. Nothing beyond mail access is requested. All three resource scopes belong to Exchange Online, so one access token serves both reading and sending.

### Sessions

- Sync, the IDLE thread and each SMTP send share the account's `MicrosoftSession`. A refresh happens under a lock and checks again after acquiring it, so concurrent threads produce one refresh. The rotated refresh token is persisted before it is used.
- `invalid_grant` ends the session and deletes the keyring item. `interaction_required`, a network failure or another server error keeps the tokens; the account shows Sign in again, or retries while offline.
- The project's session limits apply as they do for Gmail: 90 days without a refresh, or 180 days after sign-in.
- Microsoft has no endpoint for revoking one refresh token. Sign-out and removal therefore forget the tokens here, and Settings explains how to remove access in the Microsoft account. This differs from Google, where Overlook revokes the grant.
- Exchange Online ends an IMAP connection when its access token expires (`BYE … AccessTokenExpired`). The provider treats that as a lost connection and reconnects with a fresh token, and the IDLE thread does the same through its backoff.

### Sent copies

Outlook.com and Microsoft 365 file every message their SMTP server accepts in Sent Items by themselves, a moment after accepting it. This is reported by Thunderbird and MailMate users and in Mozilla bug 1431562; Microsoft does not document it, and some reports say it can be turned off. An `APPEND` of Overlook's own would leave two copies.

When Microsoft serves both the mailbox and SMTP (`ImapProvider::server_files_sent`), a send therefore looks for the server's copy by Message-ID for up to eight seconds and adopts it. Only if none appears does it append one. Microsoft 365 relaying for another provider's mailbox files nothing there, so that case appends at once. `imap::tests::microsoft_servers_file_their_own_sent_copy_so_none_is_appended` covers these cases.

### Errors that are not a bad sign-in

- `535 5.7.139 … SmtpClientAuthentication is disabled` (SMTP AUTH turned off for the organization or mailbox) is reported as that, through `smtp::SMTP_AUTH_DISABLED`. It is not shown as a sign-in problem, and no second token is tried. Microsoft's other `5.7.139` replies, such as "basic authentication is disabled", stay sign-in problems for password and OAuth accounts alike, so an Outlook.com password account still reaches **Update password** and the switch to Microsoft sign-in.
- SMTP checks that the server offers `XOAUTH2` before signing in. A `334` error challenge, which Gmail sends and lettre cannot answer, counts as a refused token.
- A refused IMAP or POP3 sign-in with a valid token usually means the organization disabled the protocol, or POP is off in Outlook.com settings. The connection test says so; Exchange gives no more detail than `AUTHENTICATE failed`.

## The app registration

Builds use `LAST_REFUGE_CLIENT_ID` in `crates/overlook-provider/src/microsoft/oauth.rs`, which is `None` until the registration exists. Packagers can set `OVERLOOK_MICROSOFT_CLIENT_ID` at build time. `~/.config/overlook/microsoft-oauth-client.json` (or `$OVERLOOK_CONFIG_DIR`) overrides both:

```json
{"client_id": "00000000-0000-0000-0000-000000000000", "tenant": "common"}
```

`tenant` is optional (`common`); `organizations`, `consumers` or a directory id or domain restricts sign-in. Settings → About shows which registration is in use. Without any registration, the setup dialog explains where to save one.

### Registering Overlook in Microsoft Entra

Only Last Refuge can do this, from an Entra tenant in its own organization:

1. Microsoft Entra admin center → App registrations → **New registration**. Name **Overlook**. Supported account types: **accounts in any organizational directory and personal Microsoft accounts**. Redirect URI: platform **Public client/native (mobile & desktop)**, value `http://localhost`. Microsoft ignores the port of loopback redirects, so this one entry covers every port Overlook picks.
2. Copy the **Application (client) ID**. There is no client secret to create.
3. **API permissions** → add the delegated permissions `IMAP.AccessAsUser.All`, `POP.AccessAsUser.All` and `SMTP.Send`, plus `offline_access`, `openid`, `profile` and `email`. Overlook requests them dynamically anyway, but administrators approving it for an organization see this list.
4. **Branding & properties**: name, logo, home page, terms and privacy statement URLs. Verify the publisher domain, then complete publisher verification as the roadmap describes. Publisher verification is free and has no license requirement.
5. Put the id in `LAST_REFUGE_CLIENT_ID`. It is public, as Thunderbird's is. Until then, test with the JSON file above.

Microsoft 365 organizations with Microsoft's default consent policies need an administrator to approve Overlook before their users can sign in; see the roadmap's administrator consent section. Personal accounts consent for themselves.

## Tests

Automated coverage, all offline (synthetic tokens, fake servers on loopback):

- `oauth::tests`: PKCE (RFC 7636 vector), callback validation, form encoding, the `XOAUTH2` layout and session limits.
- `credential::tests`: one refresh after a refused token, and no retry for passwords or other errors.
- `microsoft::oauth::tests`: registration validation (the tenant cannot inject into the URL), the authorization URL, ID-token claims, and token responses. These include rotation, `invalid_grant` and a log check that no token or trace id is written.
- `microsoft::session::tests`: four threads, one refresh; rotation persisted with the original sign-in time; a refusal refreshes unless already replaced; `invalid_grant` deletes once; other failures keep the session; disconnect; and redacted `Debug`.
- `providers::tests::only_the_newest_sign_in_of_an_account_writes_to_the_keyring`: a replaced session's refresh is not stored, its `invalid_grant` cannot delete the new sign-in, and nothing is stored after sign-out.
- `imap::tests::oauth_*` and `idle_signs_in_with_the_session_token`: a full snapshot over `AUTHENTICATE XOAUTH2` against the fake server. They cover both Microsoft-style refusals and Gmail-style error challenges, the refresh after a refused token, no `LOGIN`, nothing secret in the server log, a dead refresh token, and the IDLE thread. `microsoft_servers_file_their_own_sent_copy_so_none_is_appended` covers Sent copies.
- `pop3::tests::oauth_accounts_sign_in_with_xoauth2` and `smtp::tests::{oauth_accounts_send_with_xoauth2, smtp_auth_turned_off_by_the_organization_is_not_a_password_problem, an_xoauth2_error_challenge_is_a_refused_token}`.
- `autoconfig::tests`: Microsoft domains and the HEY and Tuta explanations.
- `microsoft::oauth::tests::id_token_claims_name_the_account_and_its_tenant`: identity matching on `email` or `preferred_username`.
- App: `loopback::tests` (Google and Microsoft redirect shapes, forgery, pollution, wrong origin, and a redirect answered on `::1`), `microsoft_signin::tests`, and `credentials::tests::microsoft_sign_ins_keep_their_registration_beside_the_tokens`.
- An independent review on 12 November 2026 covered secrets, loopback validation, session concurrency, the protocol exchanges and the dialog. It found no high-severity defects; its nine medium and low findings are fixed above.

Not verified: a real Microsoft sign-in, consent screen, token exchange, mailbox sync or send. The registration does not exist yet, and no Microsoft account was used. The native dialog has been checked by build only.

## Qualification still to do

Per the roadmap's "What earns explicit support", in the order the roadmap gives:

1. Personal Outlook.com: sign-in, consent, IMAP sync, IDLE, drafts, Sent copies (one copy, the server's, and how long it takes to appear), Archive/Junk/Deleted Items mapping, an access token expiring during IDLE, and POP3 with POP turned on.
2. Microsoft 365: user consent where allowed, the administrator-approval path, a tenant with SMTP AUTH turned off, IMAP disabled for a mailbox, and conditional access or MFA prompts on refresh.
3. Accounts whose sign-in name differs from the address (aliases, UPN), and shared mailboxes (user name set to the shared address).

## Not covered

- Outlook calendar, contacts, Focused Inbox and categories, which need a Graph provider.
- On-premises Exchange; sovereign clouds such as GCC High, DoD and 21Vianet, which have different authorities and hosts.
- Detecting Microsoft 365 custom domains from DNS. Users choose **Add Microsoft account…**; autoconfig recognises only Microsoft's own consumer domains.

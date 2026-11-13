# Proton Mail via Bridge

Written 13 November 2026 for release target 1.6.22. This is priority 5 in the [mail service provider roadmap](mail-provider-roadmap.md): Proton Mail through Proton Mail Bridge, as a guided setup entry, with a way to trust the certificate Bridge makes for itself.

Overlook does not talk to Proton's servers. Bridge is Proton's desktop app for paid plans. It runs on the same computer, keeps the mailbox decrypted locally, and serves it to mail apps over IMAP and SMTP on the loopback address. Overlook connects to Bridge like any mail app would. Proton names Thunderbird as its only officially supported Linux client and doesn't guarantee others, so Overlook must establish its own results. Nothing here implies Proton's endorsement.

## What Proton documents

From Proton's Bridge pages ([Bridge](https://proton.me/mail/bridge), [supported clients](https://proton.me/support/clients-supported-bridge), [certificate warnings](https://proton.me/support/bridge-ssl-connection-issue)), read in November 2026:

| | Server | Port | Security |
|---|---|---|---|
| IMAP | `127.0.0.1` | 1143 | STARTTLS |
| SMTP | `127.0.0.1` | 1025 | STARTTLS |

- **Credentials:** Bridge shows each account's user name and a password it generated, under the account's mailbox details. That password is not the Proton account password.
- **Settings:** the ports, and SSL instead of STARTTLS, can be changed in Bridge. Overlook's fields stay editable for that.
- **No POP3.**
- **Certificate:** Bridge makes a self-signed certificate when it is first set up, and mail apps have to be told to accept it. Bridge can export it from its advanced settings.
- **Plans:** Bridge is for paid plans only.

## Trusting Bridge's certificate

Ordinary verification refuses Bridge's certificate, because no public authority vouches for it. The roadmap rules out turning verification off. Overlook instead trusts the one certificate the person accepts, by its fingerprint, and only for a server on this computer. The code is in `crates/overlook-provider/src/pinning.rs`.

- **The pin.** `ServerEndpoint::pinned_certificate` holds the SHA-256 fingerprint (`AB:CD:…`, as `openssl x509 -fingerprint -sha256` prints it) of the certificate the person trusted. It is stored with the account's server settings. It isn't a secret.
- **Loopback only.** A pin counts only for an encrypted connection to `localhost`, `127.0.0.1`, `::1` or a `.localhost` name. A pinned connection is also refused unless the address it reached is a loopback one, in case a local-looking name resolves elsewhere. Every other server is verified against the public roots as before, pin or no pin. "Loopback" is not quite "this computer": a port forward the person set up (`ssh -L`, a container proxy) also listens on loopback, and a certificate trusted there is accepted as Bridge's would be. It is still that exact certificate, with its private key.
- **IMAP and POP3.** For a pinned local server, rustls gets a verifier that accepts exactly the pinned certificate. It still checks the handshake signature with that certificate's key, so a program that copied the certificate without its private key is refused.
- **SMTP.** lettre takes no custom verifier. For a pinned local server only, the transport tells lettre to skip the chain and name checks. lettre still verifies the handshake signature. The transport then compares the certificate received with the pin before it signs in.
- **Reading a certificate.** `pinning::fetch_certificate` connects, completes the TLS handshake (after STARTTLS, STLS or EHLO as needed), records the certificate, and disconnects without signing in. It refuses servers that aren't on this computer.
- **A changed certificate.** If a pinned server presents another certificate (Bridge reinstalled, its certificate replaced, or another program on the port), the connection is refused before any password is sent, with "the server's security certificate has changed since you trusted it". The account's status bar shows that as a sync failure. The account keeps retrying without credentials until the person reviews the change.

### In the setup dialog

1. **Add Proton account…** fills in Bridge's usual settings and asks for the Bridge password.
2. **Add account** or **Test connection** fails on both servers before signing in. The test then reads each certificate again without signing in, and the dialog shows a panel: what the certificate is, the addresses and the SHA-256 fingerprint (one line when both servers share it, as Bridge's do), and **Trust certificate**.
3. **Trust certificate** pins each certificate to its `host:port` and tests again. If the test was for saving, the account is saved when it passes.
4. Settings can't be saved without a trusted certificate. Neither **Add anyway** nor **Save anyway** is offered while an encrypted server on this computer has none, and the button stays disabled. This covers adding an account, **Server settings…** and **Update password**. It applies to a refused certificate, including one that changed, and to a server that couldn't be reached to check one before any was trusted (Bridge not running). Only **Trust certificate**, or a test that passes, saves. A server whose certificate is trusted but which is only offline (Bridge quit) can still be saved anyway.
5. Changing a server's host or port drops its pin, because a pin stays with the address it was trusted for. A changed certificate is offered as a change, with a warning not to trust it unless the change was expected. Settings → Accounts shows the start of the trusted fingerprint.

The panel appears for any server on this computer whose certificate is refused, not only Bridge. A certificate whose handshake signature fails is never offered. Only Overlook's own fixed refusals count, so a local server's error text can't bring up the panel.

First-time trust is trust on first use. If Bridge isn't running during setup, another program listening on its port would be offered instead. So the panel says to trust the certificate only while Bridge is running, and points to the certificate Bridge exports for anyone who wants to compare fingerprints.

Two related changes apply to every account. A server name that resolves to several addresses is tried address by address: `localhost` usually lists `::1` before the `127.0.0.1` Bridge listens on. An unencrypted connection, allowed only for this computer, is now refused unless it actually reaches a loopback address.

## What exists

| Area | Where | What it does |
|---|---|---|
| Model | `crates/overlook-core/src/servers.rs` | `ServerEndpoint::pinned_certificate` (omitted from stored settings when unset) and `ServerEndpoint::is_local`. |
| Pinning | `crates/overlook-provider/src/pinning.rs` | Fingerprints, `pin_for`, the pinned and recording verifiers, `fetch_certificate`, the loopback checks, `is_trust_question`, and loopback TLS fake servers for tests (`pinning::fake`, also behind the `fake-servers` feature). |
| Connections | `imap/connection.rs`, `pop3/client.rs`, `smtp.rs` | `wrap_tls` takes the endpoint and uses the pinned verifier for a pinned local server. SMTP has the pinned path and `fetch_certificate`. `secure_error::CERTIFICATE_CHANGED` is the fixed wording for a mismatch. |
| Guide | `crates/overlook-provider/src/app_passwords.rs` | `PROTON` in `GUIDES`: Proton's four address domains, Bridge's defaults, no POP3, and wording for the Bridge password. It lists no server host, because `127.0.0.1` could be any local bridge. |
| Presets | `crates/overlook-provider/src/autoconfig.rs` | The Proton row of the preset table gave way to the guide. |
| Folders | `crates/overlook-provider/src/imap/folders.rs` | An `\Archive` folder is the Archive even when an `\All` view is listed before it. |
| Setup dialog | `crates/overlook-app/src/ui/account_setup.rs` | The certificate offer and **Trust certificate** flow above. **Add Proton account…** and `OVERLOOK_SIGN_IN=proton` come from the guide list. |
| Settings | `crates/overlook-app/src/ui/settings.rs` | The connection line names the Bridge password and the trusted certificate. |

## Folders

Bridge is reported to list views beside the real folders. All Mail carries `\All`, and Starred carries `\Flagged`. It also lists the person's folders and labels under `Folders/` and `Labels/`.

- **Archive.** Overlook used to take the first `\Archive` or `\All` folder in list order as the Archive. With All Mail listed first, that would have made All Mail the Archive. An `\Archive` folder now wins, and All Mail and Starred are left out as views, as for Gmail.
- **Labels.** These are shown as folders, so a labelled message appears twice: in its folder and under `Labels/…`. Treating them as labels is left for later.

## Sent copies

Overlook files its Sent copy as for any IMAP server: it first looks in Sent by Message-ID, then appends. Proton keeps sent mail itself. Whether Bridge shows that copy before Overlook looks, or ignores the appended duplicate, is to be checked live.

## Tests

- `pinning::tests`:
  - Fingerprints.
  - Pins count only for encrypted local servers.
  - Trust questions come only from Overlook's own certificate refusals for local servers.
  - Addresses elsewhere are refused.
  - A pin on 127.0.0.2, which isn't one of the local names, is ignored.
  - Certificates are read over IMAP, POP3 and SMTP, with STARTTLS and implicit TLS, without signing in.
  - Each protocol and security mode is checked four ways against loopback TLS servers: unpinned is refused before sign-in, pinned signs in, a changed certificate is refused before sign-in, and the pinned certificate with another key is refused.
- `ui::account_setup::tests`:
  - The dialog's probe offers a fake Bridge's certificate for both servers and passes once it's pinned.
  - A changed certificate is offered as a change.
  - A certificate with the wrong key is never offered.
  - A pin stays with its address.
  - Settings need a trusted certificate before they can be saved: when the certificate is refused, changed or unreadable, and when Bridge isn't running and nothing is trusted yet. They don't need one when the password is refused, when a trusted server is only offline, or for servers elsewhere.
- `app_passwords::tests::proton_uses_bridge_on_this_computer` and `autoconfig::tests::presets_and_guesses`: Bridge's settings, the domains, and no match on `127.0.0.1` alone.
- `imap::folders::tests::proton_bridge_layout`: the Archive and the views.

The test certificates in `crates/overlook-provider/tests/fixtures/` are self-signed P-256 certificates for `127.0.0.1` and `localhost`, valid until 2126. Only tests use them.

Live, 13 November 2026: the user installed Proton Mail Bridge on Omarchy (Arch's `protonmail-bridge` package, 3.27.1), set up a real Proton account through **Add Proton account…**, trusted Bridge's certificate, and confirmed that it works. The detailed checks below were not recorded.

## Qualification still to do

These need a paid Proton plan with Bridge on Linux. Beyond that first working account:

1. **Setup.** Compare the fingerprint with the certificate Bridge exports (`openssl x509 -in cert.pem -noout -fingerprint -sha256`). Check the refusal for the Proton password.
2. **Sync.** The folder mapping (Archive, not All Mail), `Folders/` and `Labels/`, IDLE, and draft sync.
3. **Sending.** One Sent copy, and the sending identity.
4. **Bridge going away.** Bridge quit and restarted should show Offline and then recover. Bridge reinstalled, or its certificate replaced, should show the change, and **Server settings → Test connection** should offer the new certificate.
5. **Other Bridge settings.** SSL instead of STARTTLS, changed ports, and split address mode, where each address has its own credentials.

## Not covered

- Labels as labels rather than folders.
- Proton Calendar and Contacts, which Bridge doesn't serve.
- Bridge on another computer. Pins are for this computer only.
- Bridge's own certificate import. A certificate from a public authority is verified as usual and needs no pin.

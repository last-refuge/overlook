# CASA AL1 remediation plan — 13 failed controls

Assessment **CASA-23625-12721**. TAC returned it on 27 October 2026 with 13 controls marked **Failed**. This plan is a companion to the [submission handoff](casa/README.md). The original comments are in [`casa/responses.json`](casa/responses.json), the per-control evidence in [`casa/control-matrix.md`](casa/control-matrix.md), and the guidance text in [`casa/requirements.json`](casa/requirements.json). Each tester comment below is the full text from its Read More dialog.

## Status — 27 October 2026

Released in v1.6.0, with follow-ups in v1.6.1:

| Control | Change | Where |
| --- | --- | --- |
| 4.1.4 | Constant messages for TLS, certificate, network and keyring failures; detail only at debug under `overlook::diag` | `overlook-provider/src/secure_error.rs`, `credentials.rs`, IMAP/SMTP/Gmail/OAuth/updater call sites |
| 6.7.1 | Every keyring read/write/delete audited under `overlook::vault_audit` (operation, kind, account id, outcome; no secret) | `overlook-app/src/credentials.rs` |
| 6.2.1 | Release builds ignore `RUST_LOG` (fixed `overlook=info`); release panic hook prints a fixed notice; `--help` no longer advertises `RUST_LOG`; explicit `[profile.release]`; Chromium flags as one constant with a pipe-only DevTools test | `overlook-app/src/main.rs`, `Cargo.toml`, `browser_engine.rs` |
| 6.5.1 | Credential-free `overlook::signin` events for consent, loopback callback, token exchange/refresh, userinfo, revocation, IMAP/SMTP/POP3 logins | `oauth.rs`, `google_signin.rs`, `imap/connection.rs`, `smtp.rs`, `pop3/client.rs` |
| 2.2.1 | Sign out (account menu and Settings); `invalid_grant` deletes the stored refresh token; Gmail sessions idle 90 days are revoked and signed out; one shared invalidation routine with Remove account | `gmail/api.rs`, `providers.rs`, `ui/mod.rs`, `ui/sidebar.rs`, `ui/settings.rs` |
| 5.2.1 | Outgoing attachment policy (Gmail's blocked types, executable-content sniffing, zip/tar one level deep) at attach and send time; MIME type from extension; received programs/scripts are never opened | `overlook-core/src/attachment_policy.rs`, `ui/composer.rs`, `outgoing.rs`, `ui/mod.rs`, `README.md` |
| 3.1.4 | Store-level `ensure_owned` refuses any user-initiated change naming another account's message, label, draft, calendar or event; callers pass the acting account; a test drives each path with a second account's ids (v1.6.1) | `overlook-store/src/lib.rs`, call sites in `ui/` and `sync.rs` |
| 1.1.1 | Rejected credentials stop automatic sync for that account until the user signs in again, so no repeated failed logins reach the provider (v1.6.1) | `ui/mod.rs` |
| 6.5.1 | `overlook.log` created 0600 and the data folder 0700, tightening older installs (v1.6.1) | `overlook-app/src/main.rs` |
| 5.2.1 | Attachments copied into a generated folder with a cleaned name (no path, control or bidi-override characters) and 0600 permissions; 25 MB encoded cap at attach and send; ClamAV scan when installed (v1.6.1) | `attachment_policy.rs`, `ui/composer.rs`, `malware_scan.rs`, `outgoing.rs` |
| 2.2.1 | Gmail sessions also end 180 days after sign-in, however active; sign-in time kept across refreshes (v1.6.1) | `gmail/oauth.rs`, `gmail/api.rs`, `ui/mod.rs` |
| 6.1.1 | `cargo audit` in CI and before every release; a known vulnerability fails the build; JSON report published with each release (v1.6.1) | `.github/workflows/` |
| 3.1.4 | Event saves (create, RSVP, move) and contact saves refuse another account's record (v1.6.2) | `overlook-store/src/lib.rs` |
| 4.1.3 | PKCE challenge uses the RustCrypto `sha2` crate; no hand-written crypto remains (v1.6.2) | `gmail/oauth.rs` |
| 6.1.1 | Each release installs its package in a clean Arch container and attaches an SBOM, `arch-audit` report and Trivy crate scan (v1.6.2; also run for v1.6.1) | `.github/workflows/supply-chain.yml` |

Known limitations to disclose: rustls does not check certificate revocation (4.1.2); the attachment policy is a blocklist, as Gmail's is, not an allowlist (5.2.1). Owner tasks added by the remediation guidance: HSTS and a TLS 1.2 minimum on `lastrefuge.ai` in Cloudflare (4.1.1); a subdomain inventory alongside the zone export (6.4.1).

Evidence written 28 October 2026, kept out of git in `llm-docs/casa/round-2/` because it names the origin server: zone export, records screenshot, SSL Labs A+ reports for both hostnames and the subdomain inventory (4.1.1, 4.1.2, 6.4.1); crypto inventory (4.1.3); IDOR inventory (3.1.4). The secrets access policy (6.7.1) is public at `docs/secrets-policy.md`. Cloudflare now proxies `lastrefuge.ai` and `www` with Full (Strict), HSTS (1 year, includeSubDomains), TLS 1.2 minimum and Always Use HTTPS.

Still to do: live captures from v1.6.2, triage of the supply-chain reports (6.1.1), ADA list (1.1.1), resubmission.

## The pattern behind the failures

1. **We argued the control doesn't apply to a desktop app and asked TAC to decide** (4.1.1, 4.1.2, 6.1.1, 6.2.1, 6.4.1). TAC failed those rows but each comment names an acceptable artifact. Produce that artifact.
2. **The evidence was descriptive where the tester wanted something concrete**: an inventory, named algorithms, a log of the actual flow, a policy (1.1.1, 3.1.4, 4.1.3, 6.5.1, 6.7.1).
3. **Real gaps that need code** (2.2.1, 4.1.4, 5.2.1, 6.2.1, 6.5.1, 6.7.1).

Every resubmission must name the **exact shipped release**, not the unreleased v1.5.5 candidate the round-1 evidence used.

## Release alignment

- Land all code changes on `main`, cut a release (e.g. **v1.6.0**), and collect every piece of evidence from that tagged build. Record the tag, commit and binary SHA-256 in `casa/round-2/build-provenance.json`.
- Regenerate evidence with `casa/build_evidence.py` / `build_live_evidence.py` into `casa/round-2/`, leaving round 1 intact.
- Label every artifact "Overlook vX.Y.Z (released)".

## Decision needed from Jason

**5.2.1, outgoing attachments.** The tester wants either validation of outgoing attachments or a documented restriction to approved file types. A strict allowlist would break normal mail use. Recommendation: **block the file types Gmail itself refuses to send** (executables, scripts, installers; Google publishes the list), detect them by content sniffing as well as by extension, and look inside archives one level deep. Document this as the restriction. Gmail would bounce these anyway, so users lose almost nothing. The alternative, a short allowlist (documents, images, archives, calendar files), is stricter but will annoy people.

---

## 1. 1.1.1 — Authentication is resistant to brute force attacks

> The evidence identifies Google Account authentication as an external service and explains that Overlook does not collect or directly authenticate Google passwords. However, it does not confirm that Google is on the current ADA-approved external authentication services list, nor does it identify the user-selected mail providers. Citing the current ADA-approved list for the applicable provider(s) would resolve the gap.

**Plan (evidence only):**
- Find the current ADA-approved external authentication services list (the ASA-WG Web App Test Guide §1.1.1 or its linked page). Cite the Google entry with the URL and date retrieved, plus a screenshot.
- **Identify the mail providers.** Overlook supports Gmail (Google OAuth) and any IMAP/POP3/SMTP server the user enters. The tested and documented provider is **Fastmail**. If Fastmail and other IMAP providers aren't on the ADA list, say so and explain:
  - Their authentication (passwords or app passwords, MFA, rate limiting) is run by the provider, not by Overlook.
  - Overlook sends credentials only over TLS-mandatory connections (link to the 4.1.1 evidence).
  - Overlook adds no retry loop: a rejected login moves the account to "Sign in again" rather than retrying automatically. Verify this in `providers.rs` / the sync loops before claiming it, and add a backoff if it isn't true.
- If no list is publicly reachable, ask TAC in the portal reply which document they mean.

## 2. 2.2.1 — Logout invalidates all session tokens, including refresh tokens

> The evidence demonstrates a user-invoked account-removal flow that revokes the Google OAuth grant, clears tokens, and deletes local credentials. It does not show or describe a separate logout capability, nor does it establish that session expiration triggers server-side invalidation of session and refresh tokens. A code excerpt or test covering logout and expiration, including refresh-token invalidation, would resolve the gap.

**Plan (code + evidence):**
- **Add Sign out** to the account menu, separate from Remove account. Sign out:
  - shuts down the shared client
  - revokes the refresh token at Google's revoke endpoint (the existing `gmail::oauth::revoke` call in `ui/mod.rs:879`)
  - clears tokens from memory and deletes the OAuth keyring item
  - keeps the cached mail and the account row, and leaves the account in "Signed out — Sign in again"

  For IMAP/POP3 accounts, Sign out deletes the stored password from the keyring; nothing can be invalidated at the provider. Share one invalidation routine with Remove account.
- **Expiration with server-side invalidation:**
  - **Access token:** Google enforces its lifetime (`expires_in`, about 1 hour), and Overlook drops an expired token rather than reusing it.
  - **Refresh token, expired or revoked at Google** (`invalid_grant`): today this maps to `AuthError::Denied` and the "sign in required" state. Change it so that, in addition, Overlook deletes the dead refresh token from the keyring immediately. Add a test.
  - **Local session expiry (new):** if a Gmail account hasn't successfully synced for **N days** (proposal: 90, under Google's 6-month unused-token expiry), Overlook revokes the refresh token at Google and deletes it locally, leaving the account signed out. This is the "expiration triggers server-side invalidation" the tester asks for. Run the check at startup and on each sync tick, and add a test with a fake clock and a fake revoke server.
- **Tests** to include as evidence:
  - Sign out → revoke endpoint called, keyring empty, and a refresh with the old handle is refused locally.
  - `invalid_grant` → keyring entry deleted.
  - Inactivity expiry → revoke called and keyring entry deleted.
- **Live evidence** with a disposable Google account: Sign out in the UI, then replay the revoked refresh token against Google's token endpoint and show `400 invalid_grant`. Record only the status and error code, never the token. This closes round 1's "inferred, not replayed" gap.

## 3. 3.1.4 — Protection against IDOR

> The evidence identifies broad provider and local-storage categories and shows account-ID filtering in two local read queries. It does not provide a concrete inventory of user-controllable-identifier endpoints, nor does it explain the specific per-object authorization checks for the applicable create, read, update, and delete operations. A resolved endpoint inventory with corresponding authorization logic or representative source excerpts covering those operations would resolve the gap.

**Plan (evidence + small code audit):**
- Build an inventory table with one row per operation and these columns: **operation (C/R/U/D) · endpoint or command · identifier(s) · where the identifier comes from · authorization check · source reference**. Cover:
  - **Gmail REST:** messages get/modify/trash/send, threads, labels create/update/delete, drafts, attachments get, history. The check is the `users/me` path plus the bearer token's subject; Google enforces per-object ownership.
  - **Google Calendar and People:** events CRUD, contacts reads. The check is the token subject plus the calendar ACLs.
  - **IMAP/POP3/SMTP:** FETCH/STORE/COPY/MOVE/EXPUNGE by UID, APPEND, POP3 RETR/DELE, SMTP MAIL FROM. The check is the authenticated session's own mailbox.
  - **Fastmail CalDAV:** event PUT/DELETE with ETags. The check is the authenticated principal.
  - **Local SQLite:** every query that takes a message, conversation, label, draft, event or attachment id.
  - **Local inputs:** `mailto:` arguments, the OAuth loopback callback, drag-and-drop of conversations between accounts, Quick Look file paths.
- **Code audit:** grep `overlook-store` for every query keyed by an object id and confirm each also binds `account_id`, or that the id is account-scoped by construction (prefixed or a composite key). Add the bind where missing, with a test that a second account's id returns or affects nothing. Note that ids always come from stored server data, never from free text the user types.
- Present it as a table plus excerpts for one representative operation per C/R/U/D.

## 4. 4.1.1 — Enforce TLS 1.2+ with secure cipher suites

> The evidence provides source excerpts and a written description indicating that several outbound connections use HTTPS or mandatory TLS, with rustls configured for TLS 1.2/1.3. However, it explicitly states that no hosted domain or SSL Labs report exists and does not provide an equivalent dated TLS-analysis report with a hostname, protocol results, and overall grade. A dated Qualys SSL Labs or equivalent scan report for any applicable externally reachable service, or evidence establishing that no such service exists while addressing the applicable client-side assurance path, would resolve the gap.

**Plan (evidence, both paths):**
- **Server path:** run dated Qualys SSL Labs scans and export PDFs for every externally reachable service Overlook uses that Last Refuge controls: `lastrefuge.ai` and `www.lastrefuge.ai`, which host the homepage and privacy policy shown on the Google consent screen. If any scores below A, set the Cloudflare edge to a TLS 1.2 minimum, disable legacy suites, and rescan. Add Google and GitHub endpoints as supplements: `oauth2.googleapis.com`, `gmail.googleapis.com`, `api.github.com`.
- **Client path:**
  - **Dated client TLS report:** a small test binary using Overlook's production `ureq`/rustls configuration fetches `https://www.howsmyssl.com/a/check` and saves the JSON, which includes the negotiated version, the offered cipher suites and a rating. Also report the negotiated protocol for each production endpoint.
  - **Downgrade refusal:** synthetic-server tests where a TLS 1.0/1.1-only server and a TLS 1.2 server offering only CBC/RSA-key-exchange suites are both refused by the production config (IMAP, SMTP, POP3, HTTPS).
  - **Suite list:** list the enabled suites from `rustls::crypto::ring::DEFAULT_CIPHER_SUITES` (shared with 4.1.3).
- **State plainly** that Overlook itself runs no externally reachable service; the OAuth loopback listener binds 127.0.0.1 only while a sign-in is in progress. Show `ss -ltnp` during sign-in.

## 5. 4.1.2 — Trusted TLS certificates only

> The source excerpts and description indicate that production clients use Rustls with public WebPKI roots, hostname verification, and no invalid-certificate bypass, while the synthetic certificate is limited to test fixtures. However, the evidence explicitly states that no Qualys SSL Labs scan was performed, so it does not provide the required certificate-chain trust result for the public-facing server. A Qualys SSL Labs report or screenshot showing the certificate chain as trusted would resolve the gap.

**Plan:**
- Reuse the 4.1.1 SSL Labs reports for `lastrefuge.ai` / `www`. Highlight the "Certification Paths: Trusted" section.
- Supplement with **client rejection evidence** from the production configuration against the public badssl.com hosts: `expired`, `self-signed`, `wrong.host`, `untrusted-root` and `revoked`. Save a dated log showing each refused, alongside the same refusals against local synthetic servers for IMAP/SMTP/POP3.

## 6. 4.1.3 — No weak cryptography

> The evidence identifies the main cryptographic uses and shows SHA-256 and random PKCE material, with TLS and credential encryption delegated to established libraries or the operating system. However, it does not name the concrete TLS and credential-storage algorithms, provide key and IV sizes and generation details for each operation, or describe storage, rotation, and expiration for every key. A cryptographic inventory with those per-operation algorithm, parameter, and lifecycle details would resolve the gap.

**Plan (evidence only):** a full cryptographic inventory with columns **operation · algorithm · key size and generation · IV/nonce size and generation · storage · rotation · expiration · source/library version**. Rows:

| Operation | Draft content (verify every cell) |
| --- | --- |
| TLS 1.3 (all network) | Rustls 0.23.46 with ring. Suites AES-256-GCM-SHA384, AES-128-GCM-SHA256, ChaCha20-Poly1305-SHA256. Key exchange X25519 / P-256 / P-384 ECDHE, keys from the OS CSPRNG per handshake. 96-bit nonces derived per RFC 8446. Keys held in memory only, rotated every connection (KeyUpdate per rustls policy), discarded when the connection closes. |
| TLS 1.2 | ECDHE-ECDSA / ECDHE-RSA with AES-GCM or ChaCha20-Poly1305 only; no CBC, no RSA key exchange. |
| Certificate validation | WebPKI with Mozilla roots (`webpki-roots` 1.0.10). RSA ≥ 2048, ECDSA P-256/P-384, Ed25519. |
| Credential storage, host | oo7 0.4 → Secret Service (gnome-keyring or KeePassXC). Name the backend's at-rest cipher and key derivation, and the D-Bus transport session (`dh-ietf1024-sha256-aes128-cbc-pkcs7` when negotiated, otherwise plain over the user-session bus). Keys are derived from the login password and rotate when the password changes. |
| Credential storage, sandbox | oo7 file backend via Secret Portal: name the cipher, MAC, KDF, salt and IV generation from the oo7 source. |
| OAuth tokens (the secret material itself) | Access token: about 1 hour, Google-managed. Refresh token: revoked on Sign out, removal, `invalid_grant` or local inactivity expiry (2.2.1). |
| PKCE | 48-byte verifier and 24-byte state from `getrandom`; S256 challenge; single use, held in memory only, discarded after the exchange. |
| Update integrity | SHA-256 against `SHA256SUMS` fetched over HTTPS; no key. |

- Every cell must be checked against `Cargo.lock` versions and the oo7, gnome-keyring and rustls sources before submitting; the draft above is from memory.
- Show that MD5, SHA-1, DES/3DES, RC4, ECB and custom crypto are absent: a source grep plus `cargo tree -i` checks. Explain any transitive non-security use.

## 7. 4.1.4 — Cryptographic modules fail securely

> The evidence describes TLS and keyring failure handling and shows that failures stop connection or credential processing without plaintext fallback. However, the code propagates formatted underlying error text to returned errors, including TLS setup and keyring failures, so the output is not demonstrated to be uniform or free of detailed internal error information. A generic, constant user-facing error mapping and evidence of sanitized logging would resolve the gap.

**Plan (code change):**
- Introduce constant user-facing messages for every crypto failure kind and stop interpolating inner errors. Sites include:
  - `overlook-provider/src/imap/connection.rs:93-94`
  - certificate and handshake errors in IMAP, POP3 and SMTP (lettre `TlsParameters`)
  - ureq HTTPS errors in the Gmail, OAuth, updater and autoconfig clients
  - `VaultError` display strings
  - `AuthError::{Network, Protocol}` (`oauth.rs`), which currently embed `{0}` text, including Google's `error_description`

  Proposed constants: "Couldn't make a secure connection to {host}." · "The server's security certificate isn't trusted." · "Couldn't read the saved sign-in from the system keyring." · "Couldn't save the sign-in to the system keyring."
- Log the detail only to the file log, under `overlook::diag` at debug level, which release builds don't emit (see 6.2.1). Show that the detail contains no key material.
- Tests: each failure kind maps to its constant, and the user-facing string contains no rustls, oo7 or ureq text.
- Evidence: before/after excerpts, the mapping table, a screenshot of the untrusted-certificate message, and a log excerpt from the same event.

## 8. 5.2.1 — Malicious file uploads

> The evidence identifies Overlook's user-selected mail attachments as the relevant file-handling surface and shows that received files are stored with sanitized names, are not automatically executed, and require explicit external opening. However, the application intentionally accepts arbitrary attachment types, and no upload-side MIME, extension, or content validation allowlist is shown. Evidence of validation for outgoing attachments, or a documented restriction to approved file types, would be needed to resolve the gap.

**Plan (code + docs; needs the decision above):**
- Add outgoing-attachment validation in the composer (`ui/composer.rs` `attach_files` and drag-in) and again at send time, in the core, so drafts restored from storage are covered too:
  - **Extension check** against the blocked list (Gmail's published list: `.ade .adp .apk .appx .bat .cab .chm .cmd .com .cpl .dll .dmg .exe .hta .ins .isp .iso .jar .js .jse .lib .lnk .mde .msc .msi .msix .msp .mst .nsh .pif .ps1 .scr .sct .shb .sys .vb .vbe .vbs .vxd .wsc .wsf .wsh`, plus Linux `.sh .desktop .appimage .run .deb .rpm`).
  - **Content sniffing** with the `infer` crate (or similar) to catch renamed executables: ELF, PE/MZ, Mach-O, shebang scripts.
  - **One level into archives** (`.zip`, `.tar`, `.gz`) for blocked entries, as Gmail does.
  - **The existing size limit**, documented.
  - On rejection, show a constant message: "Overlook doesn't send {type} files, because most mail providers block them."
- Also on the receive side: when the user opens a received attachment that sniffs as executable, warn first, and never set the exec bit on saved files.
- Tests for each check: renamed ELF, a script with an innocent extension, a zip containing an `.exe`, and allowed types still passing.
- Documentation: a short "Attachment policy" section in the README / help, shown in the evidence.

## 9. 6.1.1 — No components with known vulnerabilities

> The evidence shows a cargo-audit 0.22.2 scan of the Rust Cargo.lock with zero reported known vulnerabilities after updates. However, it expressly excludes installed Chromium, PDF tools, and OS libraries, and provides no native-component inventory or corresponding vulnerability assessment. A distribution/package vulnerability report or inventory-based scan covering those components would resolve the gap.

**Plan (evidence + CI):**
- **Inventory:** an SBOM for the shipped package. Use `cargo cyclonedx` for the crates, plus the native runtime: everything in `packaging/PKGBUILD` `depends`/`optdepends` (Chromium, poppler/`pdftoppm`, a Secret Service implementation, Wayland/X11, fontconfig, OpenSSL if linked, and so on) with exact installed versions from `pacman -Q`.
- **Scan:**
  - **Distribution path:** `arch-audit` (Arch's security tracker) on a fully updated Arch container with the Overlook package installed. Export the report.
  - **Inventory path:** run **Trivy** or **Grype** against the same container image or SBOM, and/or OWASP Dependency-Check against the SBOM. Export dated reports.
- **Triage** each open finding in Chromium, poppler and the rest: is it fixed in the current Arch package? Is it reachable from Overlook? For Chromium the mitigations are no JavaScript, a CSP of `default-src 'none'`, no network beyond vetted remote images, a private pipe and a private profile. Present the triage as a table.
- **Unmaintained crates:** update or justify the eight unmaintained-crate warnings.
- **CI:** add `cargo audit` and the container scan to the release workflow so each release produces these reports.

## 10. 6.2.1 — Disable debug modes in production

> The evidence supports release-mode packaging and describes the absence of a production web debug console or TCP developer endpoint, with private renderer isolation. However, it does not clearly demonstrate that verbose error or diagnostic output is disabled; it expressly states that diagnostic logging can be enabled locally through RUST_LOG. A production configuration or source excerpt showing error-output suppression and the complete debug-console restrictions would resolve the gap.

**Plan (code + evidence):**
- **Logging:** in release builds (`cfg(not(debug_assertions))`), ignore `RUST_LOG` entirely and fix the filter at `overlook=info` (`main.rs:101`). Remove the `RUST_LOG` line from `--help`. Keep `safe_log_metadata` as defence in depth.
- **Error output:**
  - The constant user-facing messages from 4.1.4 cover the UI and toasts.
  - Install a release panic hook that writes "Overlook hit an internal error" plus the panic location to the log, with no payload or backtrace on stderr. Force `RUST_BACKTRACE` off in release builds.
  - Check `[profile.release]` and record its settings: `debug = false` or `"line-tables-only"`, `debug-assertions = false`, `overflow-checks` as decided, and whether to strip.
- **Debug console restrictions:** a source excerpt of the complete Chromium launch flags in `browser_engine.rs`, showing pipe-only CDP, no `--remote-debugging-port`, no `--enable-logging` or `--v`, a private `--user-data-dir`, and JavaScript disabled. A test that asserts the flag set.
- **Live evidence from the released build:**
  - `RUST_LOG=trace overlook` → the log stays at info.
  - `ss -ltnp` → no listener except the transient loopback during sign-in.
  - `--help` output.
  - a forced error showing the constant message.

## 11. 6.4.1 — No subdomain takeovers

> The evidence shows the apex and www records resolving to the same IP address and states that the owner controls the active services. However, it expressly acknowledges that the public queries are not a complete DNS-zone inventory, so the evidence does not establish that every subdomain and target has been reviewed for dangling or claimable third-party resources. A complete Cloudflare zone-record export or screenshot, together with confirmation for any third-party targets, would resolve the gap.

**Plan (Jason; needs Cloudflare access):**
1. Cloudflare → `lastrefuge.ai` → DNS → Records → **Export** (BIND file), plus a full-page screenshot of the records list.
2. Remove any record that points at a deprovisioned resource first.
3. For every record that targets a third party (CNAMEs to GitHub Pages, Vercel, Netlify, Fastly, SaaS; MX and TXT for mail), add a confirmation row: target, owning account, and proof it's active and claimed. That can be a `dig` result, an HTTP 200 from the owned site, or the provider dashboard showing the custom domain bound.
4. If Overlook's homepage, privacy policy or update URLs use any other domain, include that domain's zone too. If updates come only from `github.com/last-refuge`, say so.

## 12. 6.5.1 — No credentials in logs

> The evidence describes and shows redaction of OAuth tokens, client secrets, and PKCE verifiers, and the displayed live log contains no plaintext credentials or session tokens. However, the live excerpt contains only startup, synchronization, and mailbox activity; it does not cover the request/response in which the login credential submission occurred. The application states that it has no payment process, so payment-log evidence is not applicable; a login capture showing the credential-submission exchange, or a technically explained equivalent for the OAuth flow, would resolve the gap.

**Plan (code + new live capture):**
- **Technical explanation:** in the OAuth installed-app flow the user's password goes to Google in the system browser and never passes through Overlook. Overlook's credential exchange is the **authorization-code-for-token request**: the POST to `oauth2.googleapis.com/token` with the code, PKCE verifier and client id, and the response with access and refresh tokens. That is the request/response the tester asks for.
- **Code:** add info-level, credential-free events under `overlook::signin` for each step:
  - sign-in started
  - browser opened (redirect host only)
  - callback received (state matched, code present: yes/no, never the value)
  - token request sent (endpoint, grant type, `code=[REDACTED]`, `code_verifier=[REDACTED]`)
  - token response (status, token type, expires_in, scopes granted, `access_token=[REDACTED]`, `refresh_token=[present, REDACTED]`)
  - keyring store ok/failed
  - first sync

  The same for IMAP/SMTP password sign-in: host, mechanism and outcome, with `password=[REDACTED]`. Extend the redaction tests to cover these events.
- **Live capture** from the released build with a disposable account. Redact only the email address, and run the credential-pattern scan again. Optionally a second capture at the diagnostics level, if one is kept after 6.2.1.

## 13. 6.7.1 — Secure storage of tokens, API keys and secrets

> The evidence demonstrates that OAuth tokens and provider passwords are stored through oo7 using the OS Secret Service or encrypted Secret Portal-backed storage, with no plaintext fallback and no credential storage in configuration or SQLite. It does not provide a documented policy identifying which users, application components, or services may access each secret, nor does it show that secret access is logged or monitored. A least-privilege access policy and representative keyring access audit or monitoring evidence would resolve the gaps.

**Plan (documentation + code):**
- **Access policy document** (`docs/secrets-policy.md`, linked from the README). One row per secret: Google OAuth refresh and access token, IMAP/POP3 password, SMTP password, Fastmail app password, and the OAuth client file. Columns:
  - keyring attributes (`application`, account id, kind)
  - the Overlook component allowed to read and write it, and when (provider client creation, the sign-in flow, removal and sign-out)
  - the OS users and processes that can access it: the owning desktop user's unlocked session. Any process in that session can query the Secret Service, the documented platform model; under Flatpak, the Secret Portal restricts access to Overlook's own items.
  - lifetime and deletion triggers
  - the "never in config, SQLite, logs or crash output" guarantee
- **Least privilege in code:** route all keyring access through a single module (verify it already is). Each call names exactly the account and kind it needs, with no wildcard searches except removal's cleanup. Document that boundary in the policy.
- **Access logging:** log every keyring read, write and delete at info level under `overlook::vault_audit`: operation, kind, account id (not email), outcome, timestamp. No secret material. This target must stay outside the `oo7` filter. Add a test that the audit line appears and contains no secret.
- **Monitoring evidence:** a dated capture of `dbus-monitor --session "interface='org.freedesktop.Secret.Service'"` (and `.Item`) during sign-in, sync and sign-out. It shows which process accessed which item path, alongside the matching Overlook audit lines. Redact item labels if they contain email addresses.
- **OAuth client file:** document that `google-oauth-client.json` is user-supplied, holds a non-confidential installed-app secret (Google's documented model), lives in the user's config dir, and is enforced to mode `0600` if Overlook writes it.

---

## Work order

1. **Jason:** decide the 5.2.1 approach. Start the Cloudflare export (6.4.1) and the SSL Labs scans of `lastrefuge.ai` (4.1.1/4.1.2). Find the ADA list (1.1.1).
2. **Code (one branch, with tests):**
   - 4.1.4 constant error mapping
   - 6.2.1 release log lock, panic hook, `--help`, Chromium flag test
   - 6.5.1 sign-in events
   - 6.7.1 vault audit log
   - 2.2.1 Sign out, `invalid_grant` deletion, inactivity expiry
   - 5.2.1 attachment validation
   - 3.1.4 missing account binds
3. **Release** v1.6.0 containing all of the above.
4. **Live captures** from the released build with a disposable Google account: sign-in log (6.5.1), dbus-monitor (6.7.1), Sign out plus token replay (2.2.1), client TLS report and badssl rejections (4.1.1/4.1.2), debug-suppression checks (6.2.1).
5. **Evidence documents:** IDOR inventory (3.1.4), crypto inventory (4.1.3), SBOM and scans (6.1.1), secrets policy (6.7.1). Regenerate into `casa/round-2/`.
6. **Resubmit:** update each failed row's comment and attachments, reload to verify persistence, and record the outcome in `casa/README.md`.

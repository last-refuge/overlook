# Read Receipts (RFC 8098) & Provider Support Matrix

Overlook implements **standards-based, explicit per-message opt-in read receipts** using [RFC 8098 Message Disposition Notifications (MDN)](https://datatracker.ietf.org/doc/html/rfc8098).

---

## 1. How RFC 8098 Read Receipts Work

1. **Requesting a receipt (sender side):**
   - On any draft, reply, reply-all, forward, or resend, **Request read receipt** is **off by default** (`Draft::request_read_receipt = false`).
   - Only when the sender explicitly toggles **Request read receipt** on for that message does Overlook emit the RFC 8098 §2.1 header in the outgoing RFC 5322 MIME headers:
     ```http
     Disposition-Notification-To: <sending-account-address>
     ```
   - Both the Gmail API raw MIME upload path (`build_gmail_raw_with_font`) and the SMTP submission path used by IMAP/POP3 accounts (`build_raw_with_font` → `smtp::Transport::send`) share the exact same builder in `crates/overlook-provider/src/outgoing.rs`, so every provider receives the same compliant header.
2. **Honoring a request (recipient side):**
   - Whether a receipt is sent back depends **entirely on the recipient's mail client and mail provider policy**.
   - Many mail clients ask the recipient before sending a receipt, ignore requests silently, or allow administrators to restrict receipts to internal domains.
3. **Receiving a confirmation (MDN report):**
   - When a recipient's client honors the request, it sends back a `multipart/report; report-type=disposition-notification` message containing a machine-readable `message/disposition-notification` part with `Original-Message-ID`, `Final-Recipient`, `Reporting-UA`, and `Disposition` (for example `manual-action/MDN-sent-manually; displayed`).
   - Overlook parses incoming `message/disposition-notification` parts across both raw MIME (IMAP/POP3) and Gmail API payloads, persists the parsed metadata in SQLite (`message_read_receipts`), and displays a literal banner in the reader:
     > **Read receipt from the recipient's mail client**
   - When an incoming MDN's `Original-Message-ID` matches a sent message's `Message-ID` in the same account, Overlook also surfaces that confirmation on the sent message itself using the same truthful wording.

---

## 2. Privacy & Product Policy

### Why Overlook does NOT use tracking pixels
Some mail apps (such as Spark's automatic *Read Statuses* / open tracking) embed unique remote 1×1 tracking images (`<img>`) inside outgoing HTML mail and monitor server hits when the recipient loads images.

Overlook **never injects tracking pixels**:
- Tracking pixels fire silently without recipient awareness or consent, conflict with Overlook's own remote-image privacy posture, and produce unreliable signals under Apple Mail Privacy Protection (MPP), Gmail image proxying, and corporate image blockers.
- Adopting pixel-based open tracking would be a separate product and privacy decision, not part of standards-based read receipts.
- Overlook's read receipt UI never claims universal "opened" tracking — every confirmed result is described explicitly as a **read receipt from the recipient's mail client**.

### Incoming read receipt requests (`Disposition-Notification-To` on received mail)
- **Overlook NEVER sends read receipts automatically** when a user opens a received message carrying a `Disposition-Notification-To` header.
- Automatically transmitting an MDN leaks mailbox activity, reading time, and client fingerprint without consent (RFC 8098 §2.1 & §7 Security Considerations explicitly warn against silent automatic MDNs, especially to untrusted senders).
- If Overlook adds recipient-side receipt responses in a future milestone, it must require an explicit per-message user action (for example, a non-intrusive banner **"The sender requested a read receipt"** with **Send receipt** / **Ignore** buttons) and never auto-send by default.

---

## 3. Sending Account Provider Matrix

Status values:
- **Expected: Preserved** — provider relays standard RFC 8098 `Disposition-Notification-To` headers in submitted raw MIME / SMTP `DATA`.
- **Expected: Conditional / Policy-gated** — header preservation or native honoring depends on account tier or admin policy.
- **Live Verification** — must be checked against real accounts before shipping (see §5).

| Sending Provider / Account Type | Transport in Overlook | `Disposition-Notification-To` Header Preservation | Notes & Limitations | Live Verification |
| :--- | :--- | :--- | :--- | :--- |
| **Gmail Consumer (`@gmail.com`)** | Gmail API (`messages.send` / `drafts.send` raw RFC 2822) | **Expected: Preserved on wire (unverified on consumer relay)** | Google's *built-in Gmail web UI* restricts read receipts to eligible Google Workspace work/school accounts ([Google Support](https://support.google.com/mail/answer/9413651)). Overlook sends raw MIME containing `Disposition-Notification-To`; live test must verify whether Gmail consumer relay strips or preserves the header to external recipients. | `[ ]` Pending |
| **Google Workspace (Work / School)** | Gmail API (`messages.send` / `drafts.send` raw RFC 2822) | **Expected: Preserved (subject to Workspace Admin policy)** | Workspace Admins can allow receipts for all addresses, allowlisted domains only, or disable them entirely. Even when preserved on outbound mail, Gmail web recipients only see prompts if their own Workspace policy allows it. | `[ ]` Pending |
| **Microsoft 365 / Exchange Online** | SMTP AUTH (`smtp.office365.com`, XOAUTH2) | **Expected: Preserved** | Exchange Online preserves `Disposition-Notification-To` over SMTP submission (when SMTP AUTH is enabled for the mailbox) and natively supports RFC 8098 MDNs across Outlook clients. | `[ ]` Pending |
| **Outlook.com / Hotmail / Live (Consumer)** | SMTP AUTH (`smtp-mail.outlook.com`, XOAUTH2) | **Expected: Preserved** | Consumer Outlook SMTP preserves standard headers; recipient honoring still depends on the recipient's client. | `[ ]` Pending |
| **Fastmail** | SMTP (`smtp.fastmail.com`) | **Expected: Preserved** | Standards-compliant JMAP/IMAP/SMTP service; passes `Disposition-Notification-To` intact. | `[ ]` Pending |
| **iCloud Mail** | SMTP (`smtp.mail.me.com`) | **Expected: Preserved (verify relay)** | iCloud SMTP generally relays standard RFC 5322/8098 headers, though Apple Mail itself does not natively request or prompt for MDNs. Verify header survival end-to-end. | `[ ]` Pending |
| **Yahoo Mail / AOL** | SMTP (`smtp.mail.yahoo.com` / `smtp.aol.com`) | **Expected: Preserved (verify relay)** | Yahoo/AOL SMTP typically relays `Disposition-Notification-To`, though Yahoo webmail does not prompt recipients to send MDNs. | `[ ]` Pending |
| **Proton Mail Bridge** | Local SMTP (`127.0.0.1`, pinned TLS) | **Expected: Conditional** | Proton Bridge submits through Proton's mail backend; verify whether Proton's outbound pipeline preserves `Disposition-Notification-To` or strips optional headers. | `[ ]` Pending |
| **Generic / Self-hosted SMTP** (Postfix, Exim, Stalwart, etc.) | Direct SMTP (`STARTTLS` / `TLS`) | **Expected: Preserved** | Standard MTAs do not strip `Disposition-Notification-To`; verified locally via Overlook's SMTP integration tests. | `[x]` Automated loopback / `[ ]` Live MTA |

---

## 4. Recipient Mail Client Matrix

Even when the sending provider preserves `Disposition-Notification-To`, the recipient's mail client decides whether to prompt, auto-send, or ignore the request:

| Recipient Client | Default Behavior for RFC 8098 `Disposition-Notification-To` | MDN Format Returned | Live Verification |
| :--- | :--- | :--- | :--- |
| **Mozilla Thunderbird** | **Honors (configurable; prompt by default)** — shows banner/dialog asking whether to send a Return Receipt; user can configure per-account policy. | Standard `multipart/report; report-type=disposition-notification` + `message/disposition-notification` (`Reporting-UA: ... Thunderbird`). | `[ ]` Pending |
| **Microsoft Outlook (Classic Desktop / Exchange / M365)** | **Honors (configurable; prompt by default on Exchange/M365)** — asks user before sending a read receipt (or follows Exchange/Outlook policy). | Standard RFC 8098 MDN (`multipart/report; report-type=disposition-notification`) with `Original-Message-ID` and `Disposition`. | `[ ]` Pending |
| **Microsoft Outlook (New Outlook / Outlook Web / Mobile)** | **Honors on M365/Exchange (policy-dependent)**; Outlook.com consumer web has limited/inconsistent prompt support. | Standard RFC 8098 MDN when honored. | `[ ]` Pending |
| **Gmail Web & Mobile (Google Workspace)** | **Honors only on eligible Workspace accounts** where admin enabled read receipts; prompts recipient (*"Send receipts"* / *"Not now"*). | Standard RFC 8098 MDN (`message/disposition-notification`). | `[ ]` Pending |
| **Gmail Web & Mobile (Consumer `@gmail.com`)** | **Ignores silently** — consumer Gmail does not prompt for or send RFC 8098 read receipts. | None. | `[ ]` Pending |
| **Apple Mail (macOS / iOS)** | **Ignores silently** — Apple Mail has no built-in UI to prompt for or send RFC 8098 MDNs. | None. | `[ ]` Pending |
| **Spark Mail** | **Ignores RFC 8098 MDNs** — Spark's *Read Statuses* feature uses tracking pixels on sent mail rather than honoring incoming `Disposition-Notification-To` headers. | None. | `[ ]` Pending |
| **Fastmail Web / Roundcube / K-9 Mail** | **Mixed** — Roundcube honors with user prompt by default; Fastmail web and K-9/Thunderbird Mobile behavior varies by version/setting. | Standard RFC 8098 MDN when honored (Roundcube). | `[ ]` Pending |
| **Overlook** | **Does not auto-send** — never sends MDNs automatically without explicit consent. Parses and displays incoming MDNs cleanly. | N/A (no silent auto-reply). | `[x]` Automated tests |

---

## 5. Pre-Ship Live-Account Verification Checklist

Before enabling or advertising read receipts in a release, run the following live end-to-end checks and record the results in the tables above:

### A. Outbound Header Preservation (Per Sending Provider)
For each connected sending provider (Gmail consumer, Google Workspace, M365/Outlook.com, Fastmail, iCloud, Yahoo/AOL, Proton Bridge, generic SMTP):
1. Open a new message in Overlook and confirm **Request read receipt** is **off** by default.
2. Send a control message **without** enabling the toggle. Inspect raw source on the receiving account:
   - [ ] Confirm `Disposition-Notification-To` is **absent**.
   - [ ] Confirm the HTML body contains **zero tracking pixels** or unexpected `<img>` tags.
3. Open a new message (and also test Reply / Forward), explicitly click the **Request read receipt** receipt icon button (confirm it highlights and its tooltip changes to **Read receipt requested**), and send.
4. Inspect raw source on the receiving account:
   - [ ] Confirm `Disposition-Notification-To: <exact sending account email>` is present (or document if the sending provider stripped/rewrote it).
   - [ ] Confirm the HTML body still contains **zero tracking pixels**.
5. Save a draft with **Request read receipt** toggled on, close and reopen Overlook:
   - [ ] Confirm the toggle state persists on reopen and resets to off on the next brand-new message.

### B. Recipient Client Honoring & MDN Round-Trip
Send opted-in messages from a verified header-preserving account to:
- [ ] **Mozilla Thunderbird** (confirm prompt appears; click **Send receipt**).
- [ ] **Microsoft Outlook (M365/Exchange)** (confirm prompt appears; send receipt).
- [ ] **Google Workspace Gmail Web** (confirm prompt appears when enabled by Workspace Admin; send receipt).
- [ ] **Apple Mail** & **Gmail consumer web** (confirm no crash/noise; document silent ignore).

For every returned MDN received back in Overlook (via both Gmail API and IMAP/POP3):
- [ ] Open the incoming MDN message in Overlook's reader and verify the banner reads **"Read receipt from the recipient's mail client"** with the recipient's address and reporting client (`Reporting-UA`), and does **not** attach a raw `disposition-notification` file card.
- [ ] Open the original sent message in **Sent** (within the same account) and verify the matched receipt banner appears with **"Read receipt from the recipient's mail client"** and the receipt timestamp.
- [ ] Verify that an MDN received in Account A never appears on messages in Account B.

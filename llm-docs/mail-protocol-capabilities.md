# Mail protocol capabilities

Written 3 November 2026. Feature and data-field support across IMAP, POP3, Gmail API, Exchange (Graph/EWS), JMAP and Exchange ActiveSync. Use it to decide what belongs in the shared model, what needs a per-provider capability, and what QA has to test on each server.

## Using this doc

- **Building a feature:** find its row first. Any 🟡 or ❌ cell needs a provider capability check. Never assume a feature works everywhere because Gmail supports it.
- **QA:** check every 🟡 IMAP cell against at least Dovecot (or Fastmail), Gmail-IMAP and Exchange-IMAP. Those three behave differently on keywords, MOVE, SPECIAL-USE, CONDSTORE and IDLE. Confirm with the server's real `CAPABILITY` and `PERMANENTFLAGS` responses, not the table alone.

## Overlook today

- Providers implemented in `crates/overlook-provider/src/`: IMAP, POP3, Gmail API, CalDAV and Sample.
- Not implemented: Exchange/Graph, JMAP and EAS. Their columns are here for planning.
- The only capability gate is `supports_drafts()` in `crates/overlook-provider/src/lib.rs`.
- Gmail's Empty Folder works only over IMAP until Overlook requests the full `https://mail.google.com/` scope.

## Reading the tables

Columns:
- **IMAP**: IMAP4rev1/rev2 plus common extensions. What you get depends on the server (Dovecot, Fastmail, Gmail-IMAP and Exchange-IMAP all behave differently).
- **POP3**: download only.
- **Gmail**: Gmail REST API.
- **Exchange**: Microsoft Graph and EWS (Microsoft 365 / Outlook.com).
- **JMAP**: RFC 8620/8621 (Fastmail, Stalwart, Cyrus).
- **EAS**: Exchange ActiveSync, the mobile protocol that Exchange, Outlook.com and some Gmail Workspace setups support.

Legend: ✅ native · 🟡 partial, optional extension, or server-dependent · ❌ not available · (c) only possible client-side

## 1. Organisation & state

| Feature | IMAP | POP3 | Gmail | Exchange | JMAP | EAS |
|---|---|---|---|---|---|---|
| Folders, hierarchical | ✅ | ❌ (Inbox only) | 🟡 labels; nesting by `/` in name | ✅ | ✅ | ✅ |
| Message in several containers at once | ❌ (copy = duplicate) | ❌ | ✅ (labels) | ❌ | ✅ (`mailboxIds` set) | ❌ |
| Read / unread | ✅ `\Seen` | ❌ (c) | ✅ `UNREAD` | ✅ `isRead` | ✅ `$seen` | ✅ |
| Flag / star | ✅ `\Flagged` | ❌ | ✅ `STARRED` | ✅ flag status | ✅ `$flagged` | ✅ |
| Flag with due/start date, completed state | ❌ | ❌ | ❌ | ✅ | ❌ | ✅ |
| Answered / forwarded marker | ✅ `\Answered`, 🟡 `$Forwarded` | ❌ | ❌ (work it out from the thread) | 🟡 last-verb extended prop | ✅ `$answered` `$forwarded` | ✅ |
| Free-form tags (user-defined) | 🟡 keywords, only if `PERMANENTFLAGS` has `\*` (no on Gmail-IMAP or Exchange-IMAP) | ❌ | ✅ user labels | ✅ **categories** | ✅ keywords | ✅ categories |
| Tag colours stored on server | ❌ (Thunderbird `$label1-5` is client convention) | ❌ | ✅ label colours | ✅ master category list | ❌ (no standard) | 🟡 |
| Importance / priority (settable) | ❌ (header only, read-only) | ❌ | 🟡 `IMPORTANT` label (ML-driven) | ✅ `importance` | 🟡 `$important` keyword | ✅ |
| Focused / tabbed inbox | ❌ | ❌ | ✅ `CATEGORY_*` labels | ✅ Focused/Other | ❌ | ❌ |
| Special folder discovery (Sent/Trash/Junk…) | 🟡 SPECIAL-USE (RFC 6154), else guess by name | n/a | ✅ system labels | ✅ well-known names | ✅ roles | ✅ folder types |
| Junk / not-junk marking | 🟡 `$Junk`/`$NotJunk` + Junk folder | ❌ | ✅ `SPAM` label | ✅ Junk folder (+ report API) | ✅ role + keywords | 🟡 move only |
| Archive | 🟡 `\Archive` folder if present | ❌ | ✅ remove `INBOX` | ✅ Archive folder | ✅ role | 🟡 |
| Delete semantics | `\Deleted` + EXPUNGE, two-phase; MOVE extension optional | DELE on QUIT | Trash (30 days); permanent delete needs full `mail.google.com` scope | Deleted Items, then permanentDelete | destroy | ✅ |
| Snooze | ❌ | ❌ | ❌ (not exposed in API) | ❌ | 🟡 vendor extension | ❌ |

## 2. Sync mechanics

| Feature | IMAP | POP3 | Gmail | Exchange | JMAP | EAS |
|---|---|---|---|---|---|---|
| Stable message ID | ✅ UID + UIDVALIDITY (per folder); 🟡 `EMAILID` (RFC 8474) | 🟡 UIDL | ✅ id | ✅ id (changes on move unless immutable IDs requested) | ✅ | ✅ ServerId |
| Incremental changes | 🟡 CONDSTORE/QRESYNC, else full flag rescan | ❌ | ✅ `history.list` | ✅ delta query / SyncFolderItems | ✅ `/changes` | ✅ SyncKey |
| Push | 🟡 IDLE (one folder per connection), NOTIFY rare | ❌ | 🟡 Pub/Sub watch (needs a GCP project), else poll | 🟡 webhooks (need a public endpoint) / EWS streaming | ✅ EventSource/WebSocket | ✅ Ping |
| Threads / conversations | 🟡 THREAD extension (rare), else (c) | (c) | ✅ `threadId` | ✅ `conversationId` | ✅ `threadId` | ✅ |
| Server-side search | 🟡 SEARCH (full-text quality varies by server) | ❌ | ✅ Gmail query syntax | ✅ KQL `$search` | ✅ filter | ✅ |
| Partial fetch / attachments on demand | ✅ BODYSTRUCTURE + sections | ❌ (TOP only) | ✅ | ✅ | ✅ blobs | ✅ |
| Preview snippet from server | 🟡 PREVIEW (RFC 8970) | ❌ | ✅ `snippet` | ✅ `bodyPreview` | ✅ `preview` | 🟡 |

## 3. Sending & account settings

| Feature | IMAP | POP3 | Gmail | Exchange | JMAP | EAS |
|---|---|---|---|---|---|---|
| Send | via SMTP (separate) | via SMTP | ✅ API | ✅ API | ✅ EmailSubmission | ✅ |
| Sent copy saved automatically | ❌ client APPENDs (Gmail-SMTP is the exception) | ❌ | ✅ | ✅ | ✅ (onSuccess) | ✅ |
| Server-side drafts | ✅ APPEND + `\Draft` | ❌ | ✅ drafts resource | ✅ | ✅ `$draft` | 🟡 newer versions only |
| Scheduled send | 🟡 SMTP FUTURERELEASE (rare) | ❌ | ❌ | ✅ deferred-send prop | ✅ `sendAt` | ❌ |
| Identities / send-as aliases | ❌ | ❌ | ✅ `sendAs` | ✅ (permissions) | ✅ Identity | 🟡 |
| Server-stored signatures | ❌ | ❌ | ✅ per sendAs | 🟡 roaming signatures, no public API | ✅ Identity | ❌ |
| Filters / rules | 🟡 Sieve via ManageSieve | ❌ | ✅ filters API | ✅ messageRules | 🟡 Sieve (RFC 9661) | ❌ |
| Vacation auto-reply | 🟡 Sieve | ❌ | ✅ | ✅ | ✅ VacationResponse | ✅ OOF |
| Quota | 🟡 QUOTA ext | ❌ | ❌ | 🟡 | ✅ RFC 9425 | ❌ |
| Shared mailboxes / delegation | 🟡 ACL + NAMESPACE | ❌ | 🟡 Workspace delegates | ✅ | ✅ sharing | 🟡 |
| Read-receipt requests | header-based everywhere; IMAP `$MDNSent` records that a receipt was sent | | | ✅ dedicated flags | ✅ `$MDNSent` + MDN spec | |

## 4. Other data on the same account

| Data | IMAP stack | Gmail / Google | Exchange | JMAP | EAS |
|---|---|---|---|---|---|
| Calendar | CalDAV | Google Calendar API (also CalDAV) | Graph / EWS | 🟡 JMAP Calendars (new) | ✅ |
| Contacts | CardDAV | People API (also CardDAV) | Graph | ✅ RFC 9610 | ✅ |
| Tasks | CalDAV VTODO | Google Tasks API | Microsoft To Do (Graph) | 🟡 draft | ✅ |

## What every backend has (excluding POP3)
Folders or labels, read/unread, flag/star, drafts, send, a stable message ID, some way to do incremental sync, trash/delete, and server search.

## What only some backends have (needs a capability flag)
Multi-label membership (Gmail, JMAP), user tags/categories (IMAP only when keywords are allowed), tag colours (Gmail, Exchange), flag due dates (Exchange, EAS), importance, focused inbox, scheduled send, server signatures, rules, push without infrastructure (IMAP IDLE, JMAP, EAS), threads (native everywhere except IMAP and POP3).

**Practical mapping for "categories":**
- Exchange categories → Gmail user labels → IMAP keywords → JMAP keywords.
- Gmail-IMAP exposes labels as folders or `X-GM-LABELS`, not keywords.
- Exchange-IMAP does not expose categories at all.

---

## Appendix: every IMAP data field

### Per-message FETCH items
- **Core:**
  - `UID`
  - `FLAGS`
  - `INTERNALDATE` (when the server received the message)
  - `RFC822.SIZE`
  - `BODYSTRUCTURE` / `BODY` (MIME tree: type, params, id, description, encoding, size, lines, MD5, disposition, language, location)
  - `BODY[section]` / `BODY.PEEK[...]` (full message, `HEADER`, `HEADER.FIELDS (...)`, `TEXT`, `MIME`, part numbers, `<offset.len>` partials)
  - `BINARY[...]` / `BINARY.SIZE` (decoded parts; IMAP4rev2 / RFC 3516)
- **ENVELOPE** (parsed headers): date, subject, from, sender, reply-to, to, cc, bcc, in-reply-to, message-id.
- **Extensions:**
  - `MODSEQ` (CONDSTORE)
  - `EMAILID` / `THREADID` (OBJECTID, RFC 8474)
  - `PREVIEW` (RFC 8970)
  - `SAVEDATE` (RFC 8514)
  - `ANNOTATION` (RFC 5257, rare)
  - Gmail-only: `X-GM-MSGID`, `X-GM-THRID`, `X-GM-LABELS`
- **Everything else** (priority, List-Unsubscribe, References, Autocrypt, DKIM results, etc.) is a raw header you parse yourself; there is no server-side field for it.

### Flags and keywords
- **System flags:** `\Seen`, `\Answered`, `\Flagged`, `\Deleted`, `\Draft`, and `\Recent` (session-only; removed in rev2).
- **Registered keywords:** `$Forwarded`, `$MDNSent`, `$Junk`, `$NotJunk`, `$Phishing`, `$Important` (RFC 8457), `$Submitted`/`$SubmitPending`, `$MailFlagBit0-2` (Apple flag colours).
- **Client conventions:** Thunderbird `$label1`–`$label5`.
- **Arbitrary user keywords** work only if `PERMANENTFLAGS` includes `\*`.

### Per-mailbox
- **LIST:** name (modified UTF-7, or UTF-8 with UTF8=ACCEPT) and hierarchy delimiter.
- **LIST attributes:** `\Noselect`, `\NonExistent`, `\HasChildren`, `\HasNoChildren`, `\Marked`, `\Unmarked`, `\Subscribed`, `\Remote`.
- **Special-use roles:** `\All`, `\Archive`, `\Drafts`, `\Flagged`, `\Junk`, `\Sent`, `\Trash`, `\Important`.
- **STATUS:** `MESSAGES`, `UNSEEN`, `UIDNEXT`, `UIDVALIDITY`, `RECENT`, `HIGHESTMODSEQ`, `SIZE`, `DELETED`, `MAILBOXID`.
- **SELECT responses:** `EXISTS`, `FLAGS`, `PERMANENTFLAGS`, `UIDVALIDITY`, `UIDNEXT`, `HIGHESTMODSEQ`, `READ-WRITE`/`READ-ONLY`.
- **METADATA** (RFC 5464): `/private/comment`, `/shared/comment`, vendor keys (some clients store folder colour here).
- **ACL** (RFC 4314): rights `lrswipkxtea`.
- **QUOTA** (RFC 9208): `STORAGE`, `MESSAGE`, `MAILBOX`.

### Per-session / account
- `CAPABILITY` list
- `ID` (RFC 2971: server name/vendor)
- `NAMESPACE` (personal, other users, shared)
- `ENABLE`d extensions
- Server-level `METADATA`
- **Not in IMAP at all:** signatures, identities, rules, vacation (Sieve covers the last two), calendar, contacts.

## Possible follow-up

Replace `supports_drafts()` with a `ProviderCapabilities` struct (multi_label, keywords, tag_colors, flag_dates, scheduled_send, push, threads, server_search…). IMAP would fill it in from CAPABILITY and PERMANENTFLAGS at connect time, so the UI can hide unsupported actions instead of failing.

## Sources

- **IMAP:**
  - [RFC 3501](https://www.rfc-editor.org/rfc/rfc3501) / [RFC 9051](https://www.rfc-editor.org/rfc/rfc9051) (IMAP4rev1/rev2)
  - CONDSTORE/QRESYNC [RFC 7162](https://www.rfc-editor.org/rfc/rfc7162)
  - SPECIAL-USE [RFC 6154](https://www.rfc-editor.org/rfc/rfc6154)
  - OBJECTID [RFC 8474](https://www.rfc-editor.org/rfc/rfc8474)
  - PREVIEW [RFC 8970](https://www.rfc-editor.org/rfc/rfc8970)
  - SAVEDATE [RFC 8514](https://www.rfc-editor.org/rfc/rfc8514)
  - METADATA [RFC 5464](https://www.rfc-editor.org/rfc/rfc5464)
  - ACL [RFC 4314](https://www.rfc-editor.org/rfc/rfc4314)
  - QUOTA [RFC 9208](https://www.rfc-editor.org/rfc/rfc9208)
  - keyword registry [RFC 8457](https://www.rfc-editor.org/rfc/rfc8457)
  - [Gmail IMAP extensions](https://developers.google.com/workspace/gmail/imap/imap-extensions)
- **Gmail API:** [Gmail API reference](https://developers.google.com/workspace/gmail/api/reference/rest)
- **Exchange:** [Microsoft Graph mail](https://learn.microsoft.com/en-us/graph/outlook-mail-concept-overview)
- **JMAP:**
  - [RFC 8620](https://www.rfc-editor.org/rfc/rfc8620) / [RFC 8621](https://www.rfc-editor.org/rfc/rfc8621)
  - Quota [RFC 9425](https://www.rfc-editor.org/rfc/rfc9425)
  - Contacts [RFC 9610](https://www.rfc-editor.org/rfc/rfc9610)
  - Sieve [RFC 9661](https://www.rfc-editor.org/rfc/rfc9661)

These sources describe the protocols. Hosted servers differ from them, so record what you actually observe on a live server in that provider's qualification doc.

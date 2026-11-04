# Gmail and IMAP draft synchronization

Released in [Overlook 1.6.9](https://github.com/last-refuge/overlook/releases/tag/v1.6.9) on 2026-11-01, commit `3f0b8f1`. Schema version 19. The working version after release is 1.6.10.

## Behavior

- Drafts saved in Overlook upload to Gmail or the IMAP Drafts mailbox. Saved edits schedule a sync after a pause in typing; incoming changes also arrive on normal/manual account syncs.
- Server drafts import as editable drafts, with To/Cc/Bcc, reply headers, attachment files, and inline Content-IDs. Imported HTML stays intact when only recipients or subject change. Editing an imported body's text uses the composer's Markdown format; its original HTML is then replaced by the newly rendered body.
- Deletions propagate both ways. A remote deletion removes an unchanged, closed draft. Locally modified drafts are recovered as new drafts. A Gmail draft updated on both devices preserves the local version as a separate `(conflict copy)`; an IMAP replacement can appear as a deletion and a new UID, so the local version is recovered alongside the imported replacement.
- An open composer protects its draft from incoming replacements/deletions. Changes from another device reconcile once it closes. A stale upload acknowledgment updates only server bookkeeping, never newer text.
- Offline saves/deletions remain durable. POP3 and Sample drafts remain local.

- A recipient that is not yet a complete address stays in the local draft only (`outgoing::finished`), because autosave can upload mid-typing and Gmail refuses the whole draft with `400 Invalid To header`. It reaches the server once finished, and another device does not see it until then.
- A draft the server refuses is named in the sync error, stays local, and is retried on the next sync. The other drafts, imports and deletions in that sync still run. Other errors, such as being offline, still end the sync straight away.

## Store and reconciliation

`overlook-store/src/draft_sync.rs` keeps an independently owned `draft_sync` row for each saved draft. It survives deletion of the `drafts` row so discarding or sending during an upload cannot orphan a remote draft. The baseline contains the exact acknowledged local snapshot. Bookkeeping, timestamps, and cached Markdown HTML do not count as content edits.

Before a write, the engine commits the snapshot, its unique Message-ID, and its exact RFC 822 bytes. These persist across restart, including when staged files have been deleted after a discard. Before retrying a create/APPEND, providers look for that upload's Message-ID and compare decoded content. Another client's edit retaining the same Message-ID is a conflict, not a successful acknowledgment.

Complete listings are required before deciding a draft disappeared. Partial/failed listings, invalid descriptors and repeated page tokens are errors. Imports check the current local snapshot inside a transaction; incoming content never overwrites a concurrent autosave or queued send. Conflict copies receive independent attachment IDs and owner-only staged files in their own directories. Existing local drafts are enrolled by the migration.

Queued/sending/uncertain drafts remain under Outbox control. Only provider acceptance removes the local row. A queued offline send pauses for review if its server draft was changed, removed or sent elsewhere. A separate cleanup pass removes the server draft after acceptance and retries failed cleanup from the durable tracking row. Gmail uses `drafts.send` for an existing draft, including current MIME bytes and thread id; a missing remote draft cannot fall back to sending a new message automatically. An offline discard also preserves a server version that has changed in the meantime.

## Providers

Gmail lists every `drafts.list` page, uses the immutable draft id as identity and the changing message id as revision, and fetches raw MIME only for imports/changed versions and retry reconciliation. It uses `drafts.create`, `drafts.update`, `drafts.delete` and `drafts.send`. Received-message mapping excludes `DRAFT` messages so unsent mail does not appear as regular received mail. The existing Gmail modification scope covers these operations.

IMAP discovers `\Drafts` SPECIAL-USE first, including localized/namespaced folder paths, then conventional Draft/Drafts names. It creates Drafts on the first upload if no mailbox exists. This folder has separate draft synchronization and is excluded from the ordinary received-mail cursor. Reads use `BODY.PEEK[]`. Updates append a new `\Draft`/`\Seen` copy before deleting the old UID. A persisted upload Message-ID reconciles a lost APPEND response; UIDVALIDITY prevents stale UIDs from deleting a reset mailbox's messages. UIDPLUS deletion targets only the intended UID; without UIDPLUS, only that UID is marked `\Deleted`, leaving physical expunge to the server/other client to avoid expunging unrelated deletions.

Gmail does not expose an atomic draft compare-and-swap: the adapter checks the latest message revision immediately before updating, but simultaneous edits in the interval between that read and the PUT remain a provider limitation. IMAP replacements from other clients are correlated as new UIDs, preserving both copies rather than guessing that equal subjects identify the same draft.

Protocol references: [Gmail draft lifecycle](https://developers.google.com/workspace/gmail/api/guides/drafts), [IMAP APPEND and flags](https://www.rfc-editor.org/rfc/rfc3501.html).

## Verification

`cargo test --workspace --locked draft --no-fail-fast` exercises the engine and both real protocol clients against isolated fake services. Cases include pagination, creates/updates/deletes, absent or unfinished recipients, Bcc, thread headers, MIME/inline images, restart, lost replies, concurrent autosave/discard, open editors, conflicts, UIDVALIDITY resets, servers without UIDPLUS, and send cleanup. Run the full workspace tests and Clippy before release.

No real account is mutated by these tests. Live Gmail/IMAP and native UI testing are still needed before claiming validation against production accounts.

Final local checks on 2026-11-01: `cargo test --workspace --locked --no-fail-fast` passed 470 tests, with 33 ignored; `cargo clippy --workspace --all-targets --locked` completed with existing warnings and none in changed code. New Rust modules pass rustfmt checks, and `git diff --check` passes. Signature cleanup also has a regression test ensuring it cannot remove imported draft attachments that share a directory.

Release verification: [the 1.6.9 release workflow](https://github.com/last-refuge/overlook/actions/runs/41357785312) passed package publication and the supply-chain scan. The downloaded pacman package and generic tarball match the published SHA-256 sums, both binaries report 1.6.9, and the public latest-release endpoint and installer dry-run select this release.

# Fastmail contacts over CardDAV

Implementation record, 15 November 2026. Fastmail is the first contacts service for IMAP and POP3 accounts; the design is meant to carry to other CardDAV services. Built and tested against fixtures; the owner confirmed it works with a live Fastmail account on 15 November 2026.

## Connecting

- **New account:** the server step of a Fastmail setup has a **Fastmail contacts** switch, "Connect contacts using this mail password". It is off by default; turning it on is the person's consent to send the incoming mail password to `https://carddav.fastmail.com/`.
- **Existing account:** **Settings → Accounts → Contacts → Connect contacts**.
- **Who is offered it:** accounts whose incoming server is Fastmail's (`carddav::fastmail_contacts_server`, judged by host through the Fastmail guide, so custom domains work and an address alone never sends a password anywhere). IMAP and POP3 both qualify, since contacts are a separate server. Accounts that sign in with Microsoft don't, as there is no password to reuse.
- **Disconnect:** forgets the contacts read from Fastmail, their cards and the edits to them not sent yet. Nothing changes at Fastmail. Contacts made only on this device stay.

The connection is a row in `contact_connections` (migration 21). The password stays in the keyring as the account's incoming password.

## Syncing

- Runs inside the mail sync (`sync::sync_contacts` → `carddav_sync::run`). Queued edits are sent first. The server is checked every 15 minutes, or at once after **Sync contacts now** or connecting.
- Errors are kept for the settings page (setting `contacts_status:<account>`) and never change the mail sync's state. Contact edits of a connected account are skipped by `replay_operations`; the contacts sync sends them.
- **Discovery** follows Fastmail's shape (probed unauthenticated): 404 at the root, `/.well-known/carddav` → 301 to `/dav/addressbooks`, then the principal and its `addressbook-home-set`, then the address books. Same transport as CalDAV: HTTPS only, no proxy, the approved origin only, redirect, time and size limits.
- **Download:** a PROPFIND (Depth 1) lists each card's version; `addressbook-multiget` fetches only new and changed cards, 25 at a time. A download is complete or an error, so a failed listing never empties an address book.
- **Contacts:** one per card, id `carddav:<card URL>`, all editable. Group cards are skipped and photos aren't shown.

## Edits

- Each card's URL, ETag and the contact as last read are kept in `contact_cards` (`base_json`); the card text itself isn't stored.
- **Update:** GET the full current card, apply only the fields that differ between `base` and the edited contact (`vcard::apply_changes`), and PUT it with `If-Match`. The change works line by line on the server's text:
  - Fields Overlook doesn't model are never rewritten: other emails and phones, addresses, birthdays, photos and custom labels.
  - An edited email or phone keeps its parameters.
  - A custom display name stays unless the given or family name changed.
  - A change made elsewhere meanwhile survives, unless the same field was edited here; then the edit here wins.
- **Conflict:** a 412 means the card changed between read and write. The card is read again and the change reapplied, up to three tries.
- **Update of a deleted card:** a 404 means the card is gone at the server, and the edit recreates it.
- **New contact:** a vCard 3.0 in the default book, which is Fastmail's `Default`, otherwise the first writable book. Sent with `If-None-Match: *`.
- **Delete:** `If-Match`. If the card changed at the server since, it is deleted anyway; deleting here wins.

## Code

- `crates/overlook-provider/src/caldav/carddav.rs`: the client.
- `caldav/vcard.rs`: reading and editing vCards.
- Small extensions to the shared DAV code:
  - address-book properties in `xml.rs`;
  - `propfind_with`;
  - `typed_resource_request` for `text/vcard`.
- `crates/overlook-store/src/contact_cards.rs`: the connection and card versions. A download keeps contacts with unsent edits and doesn't bring back unsent deletions.
- `crates/overlook-app/src/carddav_sync.rs`: connect, send, download. Tested against an in-memory server.
- UI:
  - `ui/contacts.rs`: `ContactsSource`, so the Contacts page names Google, Fastmail or this device correctly.
  - The Contacts section in `ui/settings.rs`.
  - The setup switch in `ui/account_setup.rs`.

## Limits and follow-ups

- The owner's live check covered normal use. Conflicts (an edit made on another device while one waits here), a dropped connection and deletions racing an edit are covered by tests against an in-memory server.
- **Other services** (reviewed 15 November 2026):
  - **iCloud:** has CardDAV at `contacts.icloud.com` (`contacts.icloud.com.cn` in China), with an app-specific password. Discovery hands off to a per-account host such as `p42-contacts.icloud.com`. The transport refuses any origin other than the approved one, so iCloud needs a reviewed exception for Apple's contacts hosts. There are recent reports of 401s with fresh app-specific passwords.
  - **Yahoo and AOL:** `carddav.address.yahoo.com` and `carddav.aol.com` exist, but reports from 2023 onward call them unreliable or read-only. Not worth offering without testing on a real account.
  - **Proton:** no CardDAV. Bridge serves IMAP and SMTP only, so contacts stay on this device.
  - **Microsoft (Outlook.com and Microsoft 365):** no CardDAV. Contacts are in Microsoft Graph, which would need new sign-in scopes and a Graph client; that's a separate project.
  - **Gmail:** already uses the People API.
  - **Other IMAP hosts** (Nextcloud, mailbox.org, Posteo and others) often have CardDAV. A manual "another contacts server" option, with its own password like the calendars' CalDAV option, would cover them.
- No `sync-collection` yet: each check lists all versions, which is light but not minimal.
- Edits in a read-only shared book fail with a message, rather than being prevented in the form.
- **Downgrade:** migration 21 means a profile opened by this build can't be opened by 1.6.22 or earlier.

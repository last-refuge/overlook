# Contacts sync for IMAP and POP3 accounts

Implementation record, 15 November 2026.

Gmail accounts keep their contacts through the People API, which is unchanged. IMAP and POP3 accounts connect a contacts server in **Settings → Accounts → Contacts**. That server is one of:

- the CardDAV server of their mail service;
- Microsoft Graph, for accounts that sign in with Microsoft;
- any CardDAV server the person enters.

Fastmail came first, and the owner checked it with a live account on 15 November 2026. Everything else is tested against fixtures and needs a live check.

## Services

| Service | How | Sign-in | Checked |
| --- | --- | --- | --- |
| Fastmail | CardDAV, `carddav.fastmail.com` | The mail app password | Live |
| iCloud | CardDAV, `contacts.icloud.com` | The mail app-specific password, with the whole address as the user name | Fixtures |
| Yahoo | CardDAV, `carddav.address.yahoo.com` | The mail app password | Fixtures |
| AOL | CardDAV, `carddav.aol.com` | The mail app password | Fixtures |
| Microsoft (Outlook.com, Microsoft 365) | Microsoft Graph, `/me/contacts` and its folders | A sign-in of its own that allows contacts | Fixtures |
| Any other CardDAV server (Nextcloud, mailbox.org, Posteo and others) | CardDAV, at the address entered | The server's own password, or the mail password if the person switches that on | Fixtures |
| Proton | Not possible | Not applicable | Not applicable |
| Gmail | People API, as before | The Gmail sign-in | Live, earlier |

### Notes on each service

- **iCloud:**
  - Discovery hands each account off to a server of its own, such as `p42-contacts.icloud.com`.
  - The transport refuses every origin but the approved one. The single exception is `also_approved`, which applies only to iCloud connections and only to Apple's contacts hosts: `contacts` or `p<digits>-contacts`, under `icloud.com` or `icloud.com.cn`.
  - The user name is the whole Apple ID address, which the outgoing server already uses; IMAP may use the name part alone.
  - Mainland China's service, `contacts.icloud.com.cn`, is entered through the form, because the mail servers are shared.
- **Yahoo and AOL:** offered with a note that their contacts servers have refused some apps. If they refuse Overlook, mail keeps working.
- **Proton:** there is no CardDAV, Bridge serves only IMAP and SMTP, and Proton's contacts API isn't public. Contacts stay on this device.

## Consent and credentials

- **The mail password goes to a contacts server only with the person's say-so,** in one of three ways:
  - the setup switch "Connect contacts using this mail password", which is off by default and named for the service;
  - the **Connect contacts** button on the service's row in Settings;
  - "Use this account's mail password" in the form for another server, which is off by default and hidden for accounts without a password.
- **Every connection is checked before it is saved:**
  - CardDAV: at least one address book has to be found.
  - Microsoft: the contacts have to be readable with the new sign-in.
- **Keyring entries:**
  - A contacts server's own password is under `password:contacts`.
  - The Microsoft contacts sign-in is under `oauth-tokens:microsoft-contacts`, separate from the mail sign-in.
  - Disconnect deletes both, and removing the account deletes every entry it has.
- **Migration 22** adds `contact_connections.credential`, which is one of `incoming-mail`, `separate` or `microsoft`.

## Syncing

Syncing works the same way for both kinds of server:

- **When it runs:** inside the mail sync. Queued edits go first, then a complete download every 15 minutes, or at once after **Sync contacts now** or connecting.
- **Errors:** kept for the Settings page and never change the mail sync's state.
- **Downloads:** a download is complete or it is an error, so a failed listing never empties an address book.
- **Disconnect:** results are saved only into the connection the sync started with: the same server, user name and time it was made, checked inside the transaction that saves them. A sync that finishes after Disconnect, or after connecting again (every Microsoft connection has the same address), saves nothing.
- **Contact ids:** `carddav:<card URL>` or `graph:<contact URL>`. Contacts made here are `local:` until the server has them.
- **New contacts:** before one is sent, a record of where it goes and what is sent is saved as its card. If the reply is lost, the retry finds the copy the first try made, and applies only what changed here since that try, so changes made elsewhere meanwhile stay. A contact deleted here after a lost reply is deleted at the server too.
- **Another sign-in:** saving a connection to another server or sign-in first lets the earlier one's contacts, cards and waiting edits go, as Disconnect does, so nothing read from one account is sent to another. Signing in again as the same person keeps everything.

### CardDAV

- **Discovery:** the root, then `/.well-known/carddav`, then the principal and its `addressbook-home-set`, then the address books.
- **Download:** PROPFIND for the versions, then `addressbook-multiget` 25 at a time for new and changed cards only.
  - Every card that was asked for must be answered.
  - Only an explicit 404 or 410 counts as gone.
- **Edits:** the change is worked out line by line against the server's current card, and the PUT is conditional (`If-Match`, or `If-None-Match: *` for a new card).
  - Fields Overlook doesn't show are never rewritten.
  - Each name part is merged on its own.
- **New cards:** a new card's id is the local contact's id, so a retry after a lost reply finds the card the first try made at the recorded address.
- **One address, once:** changing a value to one another line already has removes that other line, so nothing is listed twice.

### Microsoft Graph

- **Token:**
  - Microsoft issues a token for one resource at a time (AADSTS28000), so contacts sign in apart from mail, with `Contacts.ReadWrite offline_access openid profile email`.
  - The sign-in uses the registration the mail sign-in came from, or else `microsoft-oauth-client.json`.
  - Personal accounts need no change to the registration, because consent is asked for at sign-in. Work or school directories may require an administrator's approval; the error says so.
- **Identity:** if the ID token names neither the account's address nor its incoming user name, the person is asked before those contacts are connected.
- **Refresh:** the access token is renewed when it is within two minutes of expiring, and the renewed sign-in is saved. A refused renewal shows **Sign in again** on the Settings row; the contacts stay.
- **Ending the sign-in:** Disconnect, Sign out, Remove account and a new contacts sign-in each end the old one first. A renewal still under way then doesn't save it back, using the same kind of generation check the mail sessions use.
- **Reads:**
  - the default folder and its subfolders, one level down;
  - every reply must carry its list of contacts, or the download is an error and the saved contacts stay;
  - only the fields shown (`$select`), 250 at a time;
  - page links are followed only within Graph.
- **Edits:**
  - A PATCH of the fields that changed, worked out against the current copy.
  - The address and phone lists are rebuilt from the server's lists, changing only the edited entry.
  - Only the shown address is edited. Clearing it removes that one address and keeps the others, and choosing one already on the contact moves it up instead of adding it twice.
  - A phone the server no longer has (changed elsewhere meanwhile) isn't looked for: the new number goes where it overwrites nothing, the mobile if it's free, else the business list.
  - The display name follows the merged names.
  - Changes made elsewhere to other fields survive. When the same field was edited in both places, the edit here wins.
- **New contacts:** POSTed to the default folder with an extended property that holds the local id. A retry looks that up first, and brings an earlier copy up to date instead of adding another.
- **Limits:** Outlook keeps three email addresses per contact. Only one phone is shown (the mobile, else the first business or home number), and an edit to it lands where it came from.

## Refused edits

An edit the server refuses for good is handled like this:

- It appears as "Couldn't sync …" through the sync outcome, and in the Settings status.
- A refused change or deletion gives way to the server's version, which is shown again.
- A refused new contact stays on this device.
- Failed operations no longer hold a contact out of downloads.
- Server errors (5xx) and connection problems are retried; they never discard an edit.

## Review of PR #29

Greptile's findings on the Fastmail pull request were fixed here, each with a test:

- A Unicode value where a scheme would end (`TEL:１２３４５`) could panic.
- A create retried after a lost reply made a duplicate.
- An edit or deletion made while a new contact uploaded was lost.
- A download that finished after Disconnect brought contacts back.
- A multiget reply that left a card out dropped it.
- A name edit overwrote the other name part changed elsewhere.
- Refused edits looked successful.

## Review of PR #30

Greptile's findings on this work, each fixed with a test:

- A CardDAV create retried after a lost reply took the earlier card as it was, losing an edit made since.
- Clearing the shown address in a Microsoft contact removed its other addresses too.
- Choosing an address the contact already had listed it twice (Microsoft, and CardDAV as well).
- An edit to a phone changed elsewhere meanwhile could overwrite the mobile number.
- A renewal in Settings hid its error, and its "signed in as someone else" question, for a connected account.
- A contacts sign-in renewed during Disconnect, Sign out or Remove account could be saved back.
- A download from an earlier connection at the same address could be saved into a new one.
- A Graph reply without its list of contacts counted as an empty address book.

A second review round found two more, also fixed with tests:

- Bringing a retried create's copy up to date compared it with the server's current copy, so a note added on a phone before the retry was taken for a local deletion. It now compares with the record of what the first try sent.
- "Use these contacts" could switch a Microsoft connection to another account and then send the first account's waiting edits with the second account's sign-in.

A third round found that a sync could still write into a connection made while it ran: the record of a new contact's upload, and the default address book, could point the new connection at the old server. Every write a sync makes is now conditional on its connection still being the account's.

## Code

- **Provider:**
  - `crates/overlook-provider/src/caldav/carddav.rs`: the CardDAV client and the service presets (`ContactsService`, `service_for_host`, `service_for_url`).
  - `caldav/vcard.rs`: vCards.
  - `caldav/mod.rs`: `also_approved`.
  - `crates/overlook-provider/src/microsoft/contacts.rs`: the Graph client, the field mapping and the PATCH builder.
- **Store:** `crates/overlook-store/src/contact_cards.rs`, shared by both: connections, versions, downloads, saving after a write, and restoring a refused edit.
- **App:**
  - `carddav_sync.rs` and `graph_contacts_sync.rs`: the two sync engines, each tested against an in-memory server.
  - `credentials.rs`: the contacts keyring entries.
  - `microsoft_signin.rs`: `start_with_scopes`.
- **UI:**
  - `ui/contacts_setup.rs`: the Settings rows, the server form and the Microsoft sign-in.
  - `ui/contacts.rs`: names the source on the Contacts page.
  - `ui/account_setup.rs`: the setup switch for Fastmail, iCloud, Yahoo and AOL.

## Limits and follow-ups

- iCloud, Yahoo, AOL and Microsoft need a check with real accounts.
- There's no `sync-collection` (CardDAV) or delta query (Graph), so each check lists everything. That is light, but not minimal.
- Edits in a read-only shared book are refused at the server, rather than prevented in the form.
- New Microsoft accounts connect contacts from Settings rather than during setup, which takes a second trip to the browser.
- **Downgrade:** migration 22 means a profile opened by this build can't be opened by 1.6.22 or earlier.

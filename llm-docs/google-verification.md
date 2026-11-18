# Google OAuth verification

Status (2026-11-17): **approved.** The client is built into release builds from the repository's Actions
secrets (`OVERLOOK_GOOGLE_CLIENT_ID`, `OVERLOOK_GOOGLE_CLIENT_SECRET`); see `docs/secrets-policy.md`. The
yearly CASA reassessment for the restricted scopes still applies. The rest of this page is the submission
record.

Status (2026-10-12): preparing the submission for project `overlook-511415` (Desktop app client).
Sensitive scopes: `gmail.send`, `calendar.events`, `calendar.readonly`, `contacts` (read-write, 2026-10-13:
the Contacts module adds, edits and deletes address-book entries through the People API; it replaced
`contacts.readonly`), `contacts.other.readonly` (2026-10-12, the "other contacts" list for recipient
suggestions). Restricted scopes:
`gmail.modify` and `gmail.settings.basic` (added 2026-10-12 for automatic replies; read and write of the
vacation responder only). Both need the yearly CASA Tier 2 security assessment in addition to the review.
The justification texts below predate the three new scopes; add a sentence for each before submitting.

## Scope justification (sensitive-scopes field, under 1,000 characters)

```
Overlook is a native Linux desktop mail and calendar client. It calls Google APIs directly from the user's computer and stores data only in a local database on that device; we run no servers and never receive user data.

gmail.send: sends a message only when the user clicks Send on one they composed (new, reply, forward) via users.messages.send. No narrower scope allows sending.

calendar.events: lists events (events.list with sync tokens) for Day/Week/Month/Agenda views; accepts, tentatively accepts, or declines invitations (events.patch); creates, edits, and deletes events the user enters. The readonly and .owned variants would block RSVPs and shared calendars with write access.

calendar.readonly: one call, users/me/calendarList, to learn which calendars exist (name, color, access role, primary, default reminders). calendar.events does not grant this. Nothing is written.

Data serves only these user-facing features, never ads, third parties, or training.
```

## Restricted scope (gmail.modify)

```
Overlook is a full mail client, so it reads the user's mail (users.messages.list/get, users.history.list for incremental sync, users.labels.list, attachments.get when the user opens an attachment) and files it: read/unread, star, archive, move to a label, Trash, Junk, and the reverse. Filing uses users.messages.batchModify, which changes a message's labels and requires gmail.modify. gmail.readonly cannot change labels and gmail.labels only manages label definitions, so gmail.modify is the narrowest scope for a client that reads and organizes mail. Overlook never permanently deletes mail and never requests mail.google.com. Mail is cached only on the user's device; nothing is uploaded.
```

## Demo video

Recorded 2026-10-12 with `scripts/record-demo.sh`'s approach (gpu-screen-recorder region capture of a
floating 2048×1152 logical window on an empty Hyprland workspace, driven with ydotool + wtype), then cut
and captioned with ffmpeg. Final: `~/Videos/overlook-oauth-demo.mp4` (3 min 18 s, 2560×1440); raw take
kept alongside. It shows: the Credentials page with the client ID, Add Gmail account, the account chooser
with the client ID in the URL, the unverified-app warning, the consent screen with each scope, the
loopback completion page, reading mail, star/unread/archive/move to Inbox, sending a message, calendar
list and views, create/edit/delete of an event, and Settings → About.

Still needed before submitting: a privacy policy and homepage on a verified domain (lastrefuge.ai), the
Limited Use disclosure sentence on that page, the YouTube link (unlisted), and the CASA assessment for
`gmail.modify`. Consider switching `calendar.readonly` to the narrower `calendar.calendarlist.readonly`
(one-line change in `oauth.rs`; users sign in again once).

## Notes from the recording

- Google shows "You're signing back in to Overlook" first when scopes were granted before; the full
  scope list appears after Continue. Our request already sends `prompt=consent`.
- Two bugs found and fixed the same day: calendar single-letter shortcuts fired while typing in the event
  editor (now propagated to the text field), and a locally deleted event reappeared until the next sync
  because the download ran before the operation replay (events with pending operations are now protected).

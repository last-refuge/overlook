# Multi-account and mail workflow plan

BitcoinGrower tried Overlook 2.1.0 with multiple Google accounts and asked for a combined inbox, working account switching, a combined calendar view, Gmail shortcuts, email snooze, and read receipts. The proposed first milestone is account switching and **All Inboxes**. Email snooze comes next because it is a workflow they still use in Spark. [Overlook 2.1.0 release thread](https://x.com/jasona/status/2124551341498609724)

## 1. Make account switching explicit

Turn the top-right avatar into an account menu. Show each account's name and address, check the active account, and include a separate **Manage accounts** item for the existing settings screen. Reuse the same switch action as the sidebar. The avatar currently opens Accounts settings. [Current header](/crates/overlook-app/src/ui/shell.rs#L213) · [Account switching feedback](https://x.com/BitcoinGrower/status/2124983367565459484)

**Acceptance.** With two Google accounts connected, switch in both directions. The checked account, header, inbox, and default From address should all reflect the selection. Verify that the selected account is restored on restart.

## 2. Add All Inboxes

Add an opt-in **All Inboxes** entry in the Mail sidebar. Show conversations from connected inboxes in one newest-first list, with the owning account visible on each row. Keep each thread attached to its account, even when similar mail appears in both. The conversation model already carries the account ID. [Conversation model](/crates/overlook-core/src/model.rs#L411) · [Unified inbox feedback](https://x.com/BitcoinGrower/status/2124982609788985618)

**Acceptance.** Mail from both accounts appears in timestamp order, including cached mail while offline. Replies use the thread's account. Archive and delete act on the owning account, including when a selection spans accounts. A new message opened from **All Inboxes** shows an editable **From** field before sending.

## 3. Add email snooze

Add **Snooze** to a conversation with presets and a custom date and time. A snoozed conversation leaves Overlook's inbox views and appears under **Snoozed**, where the user can change the time or cancel it. It returns to the inbox when due or when a new inbound reply arrives. Start with snoozes stored on the device. Treat cross-device sync as a separate product decision. [Email snooze feedback](https://x.com/BitcoinGrower/status/2124983920345383080) · [Spark snooze behavior](https://sparkmailapp.com/help/tips-tricks/snooze-emails)

**Acceptance.** A snooze applies to the correct account and survives restart. A conversation returns once at the deadline or on a new reply. Cancelling immediately returns it, and rescheduling uses the new time. Verify that reconnecting and syncing a conversation does not resurrect an expired snooze or mistake an existing reply for a new one.

## 4. Expand Calendar and keyboard settings

Ship these as independent changes.

- **Calendar.** Add an account filter with an **All Calendars** option. Show the source calendar in event details and the destination calendar when creating an event. Verify that the combined view includes visible calendars from both accounts and that edits go to the event's owning calendar. [Calendar feedback](https://x.com/BitcoinGrower/status/2124983367565459484)
- **Keyboard.** Add a Gmail preset and per-action overrides with conflict detection and reset. Reuse existing mail actions where the shortcuts already align, including `j`, `k`, and `e`. Verify that single-key commands only fire in the appropriate mail context and that switching presets updates the shortcut hints. [Shortcut feedback](https://x.com/BitcoinGrower/status/2124982609788985618) · [Gmail keyboard shortcuts](https://support.google.com/mail/answer/6594)

## 5. Define read receipts and verify provider support

Start by evaluating an opt-in **Request a read receipt** action in compose. A standards-based request depends on the recipient's mail client and policy. Gmail's built-in feature is limited to eligible work and school accounts. Spark's automatic open tracking uses pixels, so adopting that approach would be a separate product decision. [Read receipt feedback](https://x.com/BitcoinGrower/status/2124983920345383080) · [Receipt standard](https://datatracker.ietf.org/doc/html/rfc8098) · [Gmail read receipts](https://support.google.com/mail/answer/9413651) · [Spark Read Statuses](https://sparkmailapp.com/help/spark-for-teams/read-statuses)

**Acceptance before shipping.** Test which sending accounts preserve a receipt request and which recipient clients honor it. Make the request explicit per message and describe any confirmed result as a receipt from the recipient's client.

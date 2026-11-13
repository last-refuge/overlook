# Overlook Documentation

Overlook is a native Linux mail client in Rust, with an Outlook-inspired workflow and an alpine identity in the Last Refuge family.

- [Session handoff](session-handoff.md): comprehensive context and the recommended continuation milestone for another session.
- [Foundation implementation plan](foundation-implementation-plan.md): validated technical foundation (Rust + GPUI), tradeoffs, workspace rules, milestone sequence, and status log.
- [Native app guide](overlook-native-app.md): building, running, testing, architecture, keyboard shortcuts, and known gaps of the GPUI application in `crates/`.
- [Draft synchronization](draft-sync.md): two-way Gmail/IMAP drafts, conflicts, durable uploads/deletions, MIME attachments, and retry recovery.
- [Mail protocol capabilities](mail-protocol-capabilities.md): feature and data-field support across IMAP, POP3, Gmail, Exchange, JMAP and ActiveSync; what to gate per provider and what to QA per server.
- [Mail service provider roadmap](mail-provider-roadmap.md): prioritized provider support, existing presets and qualification gaps, Microsoft verification and administrator consent requirements, and services named by competing clients.
- [Microsoft accounts](microsoft-mail.md): Outlook.com and Microsoft 365 over IMAP/POP3/SMTP with Microsoft sign-in (XOAUTH2): design, sessions, Sent copies, the Entra app registration, tests, and what remains to qualify.
- [Time zones](time-zones.md): date-aware mail and calendar times, the Settings override, system-change prompts, all-day projections, and regression checks.
- [Product and implementation plan](native-linux-mail-client-plan.md): phased provider support, architecture, offline synchronization, Light/Dark theming, and release criteria.
- [IMAP and POP3 implementation plan](imap-pop3-implementation-plan.md): phase 2 and 3 design fitted to the current provider trait, sync engine, store, and settings UI; milestones M0 to M5.
- [Calendars for IMAP accounts](imap-calendar-implementation-plan.md): Fastmail-first CalDAV discovery and setup, independent calendar sync, offline edits, recurrence and conflicts, with a follow-up for invitations and RSVP.
- [Calendar provider roadmap](calendar-provider-roadmap.md): shared CalDAV implementation, Fastmail delivery sequence, and qualification order for Nextcloud, iCloud, mailbox.org, GMX/WEB.DE, Yahoo, and Zoho.
- [Fastmail calendar read sync](fastmail-calendar-read-sync.md): account setup, cached offline viewing, server-expanded recurrence, atomic snapshot synchronization, and current limitations.
- [Fastmail calendar writes](fastmail-calendar-write-sync.md): personal-event CRUD, durable offline changes, conditional uploads, conflict review, recurrence limits, and validation.
- [Fastmail onboarding and autonomous workflows](fastmail-onboarding-and-workflows.md): optional calendars during account setup and isolated HTTPS/SQLite tests for offline recovery, conflicts and lost replies.
- [Fastmail personal-calendar completion](fastmail-calendar-completion.md): current scope, collection sync tokens, default-calendar settings, independent Radicale verification, final checks and remaining qualification limits.
- [Fastmail live qualification](fastmail-live-qualification.md): observed hosted-service results and incomplete checks, kept separate from simulated tests.
- [Google data flow review](google-data-flow-review.md): source evidence, privacy qualifications, test results, and architecture wording for the CASA applicability discussion.
- [CASA AL1 assessment handoff](casa/README.md): the submitted 48-control assessment, 50 evidence images, live verification, security fixes, submission confirmation, and assessor/release follow-up items.
- [CASA remediation plan](casa-remediation-plan.md): plan for the 13 controls TAC failed on 27 October 2026.
- [Brand system](overlook-brand.md): the selected identity, positioning, logo usage, colors, typography, voice, and product design guidance.
- [Brand asset guide](overlook-brand-assets.md): the asset inventory, implementation handoff, regeneration instructions, and archive packaging.
- [Art direction](overlook-art-direction.md): original landscape artwork provenance and exact generation prompt.

Application code lives in [`crates/`](../crates/), packaging drafts in [`packaging/`](../packaging/), and helper scripts in [`scripts/`](../scripts/). Reusable assets and source files are in [`branding/`](../branding/). Start with the [visual brand book](../branding/index.html), [mail design preview](../branding/mail-preview.html), or [portable brand kit](../branding/overlook-brand-kit.zip).

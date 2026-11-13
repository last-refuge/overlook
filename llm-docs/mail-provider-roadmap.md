# Mail Service Provider Roadmap

Written 12 November 2026; status updated the same day. This document records the recommended order for expanding Overlook's explicitly supported mail services, the implementation baseline, research into other clients, and Microsoft's verification requirements.

The priority order assumes broad consumer and small-business coverage while retaining Overlook's focus on a native Linux desktop experience. It is a product recommendation, not a ranking based on measured Overlook demand or provider market share. Revisit it as users request services and real-account compatibility is established.

Mail support does not imply calendar, contacts, or meeting support. Those capabilities have separate requirements; see the [calendar provider roadmap](calendar-provider-roadmap.md).

## Current implementation

- **Gmail:** a dedicated Gmail API provider and Google sign-in already exist. Validate Google Workspace accounts and organization policies through this integration rather than introducing a second Google provider. Google verification and CASA work are tracked separately in the [CASA assessment handoff](casa/README.md) and [remediation plan](casa-remediation-plan.md).
- **IMAP and SMTP:** receiving and sending implementations exist, with TLS/STARTTLS, password authentication, account setup, and stored credentials. Protocol availability is not proof of compatibility with a particular hosted service.
- **POP3:** implemented, including local message storage, UIDL tracking, retention policies, and SMTP sending. It is available in account setup.
- **Microsoft:** milestone 1 is implemented: IMAP or POP3 and SMTP accounts that sign in with Microsoft (OAuth 2.0, SASL `XOAUTH2`) instead of a password. See [Microsoft accounts over IMAP, POP3 and SMTP](microsoft-mail.md). It is tested against scripted servers only. The Last Refuge app registration does not exist yet, and no real Microsoft account has signed in.
- **Discovery:** built-in presets are tried first, then Thunderbird's ISPDB, then an unverified server-name guess. An automatically discovered endpoint does not establish compatible authentication or account eligibility.

The built-in IMAP presets name Fastmail, iCloud, Yahoo, AOL, Zoho, Proton Mail Bridge, Posteo, mailbox.org, GMX, WEB.DE, and Gmail. Outlook.com, Hotmail, Live and MSN addresses now resolve to Microsoft sign-in. The misleading HEY and Tuta entries were replaced with explanations that the services cannot be added. POP3 presets cover Fastmail, Yahoo, AOL, Zoho, Posteo, mailbox.org, GMX, WEB.DE, and Gmail. These are configuration entries, not a verified service-support list.

Source references: [provider kinds](../crates/overlook-core/src/model.rs), [server settings](../crates/overlook-core/src/servers.rs), [presets and discovery](../crates/overlook-provider/src/autoconfig.rs), [provider construction](../crates/overlook-app/src/providers.rs), [SMTP](../crates/overlook-provider/src/smtp.rs), and [POP3](../crates/overlook-provider/src/pop3/mod.rs).

During the review preceding this document, focused provider tests passed:

- `cargo test -p overlook-provider pop3 --lib`: 15 passed, 1 ignored.
- `cargo test -p overlook-provider smtp --lib`: 11 passed, 1 ignored.

The ignored tests require real-server configuration and were not run. These results demonstrate automated coverage, not hosted-service qualification. The repository README still labels IMAP/SMTP and POP3 beta. Existing [Fastmail live qualification](fastmail-live-qualification.md) records calendar observations and must not be treated as proof of complete mail qualification.

## Recommended order

| Priority | Service | Existing foundation | Work needed for explicit support |
|---|---|---|---|
| 1 | Microsoft: Outlook.com, Hotmail, Live, and Microsoft 365 / Exchange Online | Shared mail model, IMAP/SMTP engine, keyring and sync infrastructure | Microsoft sign-in, token refresh, protocol authentication or a dedicated API adapter, provider setup, and personal/business account qualification. |
| 2 | Apple iCloud Mail | IMAP/SMTP preset | App-password guidance, account setup, aliases/custom-domain qualification, and real receiving, sending, and folder checks. |
| 3 | Yahoo Mail and AOL | IMAP/POP3/SMTP presets | Provider-specific authentication guidance, OAuth integration where available, and separate Yahoo/AOL qualification. Qualify related ISP accounts individually. |
| 4 | Fastmail | IMAP/POP3/SMTP presets and separate CalDAV work | Guided app-password setup, custom-domain and alias checks, folder behavior, and mail-specific qualification. |
| 5 | Proton Mail through Proton Mail Bridge | Local IMAP/SMTP preset | Bridge-specific credentials and ports, certificate trust handling, local availability/restart recovery, and Linux interoperability qualification. |
| 6 | Zoho Mail | IMAP/POP3/SMTP presets | Account/plan eligibility guidance, regional endpoints, custom domains, personal versus business settings, and real-account qualification. |
| 7 | GMX, WEB.DE, mailbox.org, and Posteo | IMAP/POP3/SMTP presets | A batch of provider-specific setup and interoperability work; promote individual services earlier if users request them. |

### 1. Microsoft

**Status (12 November 2026):** milestone 1 is built: IMAP/SMTP with OAuth, chosen over a Graph adapter, and a built-in client id with a file override. Next steps are the Entra registration, then the qualification list in [microsoft-mail.md](microsoft-mail.md).

Microsoft is the most consequential missing integration in this roadmap. Cover personal Outlook.com/Hotmail/Live accounts first, then qualify Microsoft 365 organizations, including their consent and access policies.

OAuth for IMAP/SMTP is a route that reuses the current engine. Microsoft documents OAuth for both Outlook.com and Microsoft 365, including delegated scopes, refresh tokens, and SASL XOAUTH2. A Graph-based adapter is an alternative architecture; this roadmap does not commit to one or claim that either bypasses tenant policy. [Microsoft OAuth documentation](https://learn.microsoft.com/en-us/exchange/client-developer/legacy-protocols/how-to-authenticate-an-imap-pop-smtp-application-by-using-oauth)

Include expired/revoked tokens, account removal, administrator approval messages, and reconnect behavior in the implementation. Treat on-premises Exchange as separate future work rather than implying it is covered by Exchange Online support.

### 2. iCloud

**Status (13 November 2026):** milestone 1 is built and a real account works: guided app-specific password setup, Apple's user names with a fallback, an "Add iCloud account…" entry that covers custom domains, and POP3 setups switching to IMAP. A reusable `AppPasswordGuide` is meant to serve Yahoo, AOL, Fastmail and Zoho next. Real-account qualification is listed in [icloud-mail.md](icloud-mail.md).

iCloud is a relatively small extension of the existing infrastructure and useful for people using Linux alongside Apple devices. Provide a clear app-specific password setup path and verify username handling, aliases, custom domains, Sent/Trash mapping, and sending identity.

Apple documents IMAP and SMTP access with an app-specific password. iCloud does not support POP. [Apple server settings](https://support.apple.com/en-us/102525)

### 3. Yahoo and AOL

**Status (13 November 2026):** step 1, guided app-password setup for both, is built: Yahoo and AOL guides with POP3, "Add Yahoo account…" and "Add AOL account…", and spaces taken out of pasted app passwords. A real Yahoo account works; AOL is checked by build and tests only. Step 2, OAuth, is not started. See [yahoo-aol-mail.md](yahoo-aol-mail.md).

Group the implementation effort while maintaining separate compatibility results. Improve the current password setup first, then implement browser-based OAuth where provider registration and access permit it. Do not assume that Yahoo, AOL, AT&T, Verizon, or every Yahoo-hosted ISP address share identical onboarding requirements.

Thunderbird explicitly documents OAuth for Yahoo, AOL, and AT&T, establishing a precedent for provider-specific sign-in support. It does not automatically grant Overlook access to the same registrations. [Thunderbird authentication documentation](https://support.mozilla.org/en-US/kb/thunderbird-and-yahoo)

### 4. Fastmail

Fastmail is a good fit for the intended audience and a useful standard-protocol qualification target. Keep the initial mail work on IMAP/SMTP; a dedicated JMAP adapter can be evaluated after the existing experience is dependable.

Every third-party client needs its own app password. Fastmail's documented Basic plans do not include third-party protocol access. Setup should explain the requirement rather than repeatedly reporting an invalid password. [Fastmail app passwords](https://www.fastmail.help/hc/en-us/articles/360058752854-App-passwords)

Existing CalDAV implementation and live observations remain governed by the calendar documents. Mail priority 4 does not replace the separate Fastmail-first calendar delivery sequence.

### 5. Proton Mail via Bridge

Present this service as **Proton Mail via Bridge**, not a direct Proton server integration. The current preset is only a starting point: users need Bridge-generated credentials, its actual local endpoints, and an appropriate certificate trust path.

The current [IMAP TLS configuration](../crates/overlook-provider/src/imap/connection.rs) trusts public WebPKI roots. Bridge uses a locally generated self-signed certificate, so the trust flow needs explicit implementation and validation for both receiving and sending. Do not solve this by globally disabling certificate verification. [Proton certificate guidance](https://proton.me/support/bridge-ssl-connection-issue)

Qualify Linux first, including Bridge being stopped, restarted, or reconfigured. Proton lists Thunderbird as its officially supported Linux client and says compatibility with other clients is not guaranteed. Overlook must establish its own results without implying Proton endorsement. [Proton-supported clients](https://proton.me/support/clients-supported-bridge)

### 6. Zoho

Zoho adds a useful small-business option, but account recognition cannot depend only on a `zoho.com` address. Validate custom domains, regional data centers, and account-specific endpoint choices. The existing EU-domain presets should not be assumed correct merely because they match an address.

Zoho documents different endpoints for personal and paid organization accounts, possible app-password requirements, administrator-controlled IMAP access, and no IMAP access for newly created free-plan accounts. Use the settings shown for the user's account. [Zoho IMAP/SMTP settings](https://www.zoho.com/mail/help/imap-access.html)

### 7. GMX, WEB.DE, mailbox.org, and Posteo

Use the existing shared protocol engines and presets. The remaining work is service-specific authentication guidance, account eligibility checks, folder behavior, and hosted-service validation. Record results separately for each provider and account type rather than declaring all four supported after testing one.

## Microsoft verification and approval requirements

As of this research, Microsoft's published delegated-mail integration requirements do **not** impose a general CASA-style external security assessment simply to add Microsoft mail access. This conclusion concerns a standalone desktop client, not a promise about every marketplace program or customer's procurement requirements.

### App registration and OAuth

Register Overlook with Microsoft Entra, configure the supported account types and desktop redirect behavior, and request the delegated permissions needed for the chosen mail implementation. Microsoft documents the registration, token acquisition, and connection-authentication steps. [Mail OAuth integration](https://learn.microsoft.com/en-us/exchange/client-developer/legacy-protocols/how-to-authenticate-an-imap-pop-smtp-application-by-using-oauth)

### Publisher verification

Plan to verify the publishing organization and associate its verified Microsoft AI Cloud Partner Program identity with the app registration. The process involves Partner Center verification, matching domain ownership, appropriate account roles, and MFA.

Publisher verification is free and has no license requirement. It establishes publisher identity; it does not certify Overlook's code or security controls. Include it in release preparation without presenting it as a universal prerequisite for every development or personal-account sign-in. [Publisher verification overview](https://learn.microsoft.com/en-us/entra/identity-platform/publisher-verification-overview)

### Microsoft 365 Certification

Microsoft offers a separate security certification program with an independent paid audit and annual recertification. It is not a general prerequisite for ordinary delegated OAuth mail access. Evaluate applicability and eligibility only if a distribution channel, customer requirement, or product decision justifies pursuing the certification. A standalone desktop app should not assume it qualifies for every Microsoft 365 certification program. [Certification framework](https://learn.microsoft.com/en-us/microsoft-365-app-certification/docs/certification)

### Business-account administrator consent

This is a concrete rollout constraint. Microsoft's current managed default consent policy excludes ordinary user consent for mail-reading permissions, including Graph `Mail.Read`/`Mail.ReadWrite` and Exchange Online `IMAP.AccessAsUser.All`/`POP.AccessAsUser.All`.

An additional default policy allows specific established mail applications, including Apple Mail, Spark, eM Client, Samsung/Android Mail, and Thunderbird. Overlook is not on the published list. For organizations using these defaults, plan for administrator consent unless another policy allows the user to consent. Publisher verification alone does not provide an exception. Choosing Graph instead of IMAP does not remove this issue. [Microsoft's current consent policies](https://learn.microsoft.com/en-us/entra/identity/enterprise-apps/manage-app-consent-policies)

This business-tenant policy is separate from personal Outlook.com accounts. The rollout should provide a clear administrator approval path and avoid promising universal self-service setup for Microsoft 365 users.

## Deferred services and preset corrections

- **On-premises Exchange:** defer until concrete demand warrants its separate protocol, authentication, and deployment requirements. It is distinct from Microsoft 365 / Exchange Online.
- **Specialist enterprise platforms:** Kerio, IceWarp, SmarterMail, MDaemon, Imageway, SOGo, and similar systems can be evaluated through shared protocols or later integrations. Do not prioritize dedicated adapters without user demand.
- **Other named services:** Mailfence, Variomedia, Yandex, mail.com, 163.com, and QQ appeared in competitor documentation. Keep them as future discovery/qualification candidates, without inferring Overlook support from another client's compatibility list.
- **HEY (done 12 November 2026):** the `imap.hey.com` / `smtp.hey.com` preset is gone; adding a hey.com address explains that HEY lets no other mail app in (`autoconfig::unsupported`). HEY states that off-the-shelf third-party email apps cannot access its mail. [HEY FAQ](https://www.hey.com/faqs/)
- **Tuta (done 12 November 2026):** the invented endpoints and "paid bridge" description are gone; Tuta addresses get the same explanation. Tuta's official documentation says it does not provide IMAP/POP3/SMTP access. Any separately documented add-on or third-party bridge would require its own investigation. [Tuta support](https://tuta.com/support/howto)

## What earns explicit support

A service should be described as supported only when its declared account types have clear setup instructions and recorded compatibility evidence. Track these independently:

1. Endpoint discovery and authentication, including plan restrictions, app passwords, OAuth renewal, and administrator approval where applicable.
2. Receiving and sending, message bodies and attachments, sender identity, aliases, and custom domains within the advertised scope.
3. Folder and message changes, drafts, deletion/retention behavior, and Sent copies appropriate to the protocol.
4. Offline/restart recovery, rejected credentials, server errors, and interrupted or uncertain sends.
5. Hosted-service observations, kept separate from scripted protocol tests and from calendar/contact qualification.

Do not ask the user to repeat guided live-test scripts merely to complete this roadmap. Existing project instructions leave acceptance testing to normal use; arrange further real-account testing only within the authorization available at the time.

## Reference: services named by other clients

These are services explicitly named in the official documentation checked on 12 November 2026. They are not exhaustive compatibility lists, and omission does not prove incompatibility. A named provider may still use standard IMAP/SMTP rather than a dedicated API integration. Platform and account restrictions apply.

| Client | Named services and material qualifications | Sources |
|---|---|---|
| Apple Mail | iCloud; Google/Gmail; Yahoo; AOL; Outlook.com/Hotmail/Live/MSN; Microsoft Exchange. Its documentation also maps ISP addresses, including AT&T, Cox, Frontier, Rogers, and Verizon, to Yahoo/AOL setup. Outlook accounts use the Exchange option on Mac. | [Provider selection](https://support.apple.com/en-us/102088) |
| New Outlook for Windows | Microsoft 365 work/school accounts (Exchange Online); Outlook.com/Hotmail; Gmail; Yahoo; iCloud; generic IMAP/POP. On-premises Exchange is explicitly unsupported in this version. | [Supported account types](https://learn.microsoft.com/en-us/microsoft-365-apps/outlook/get-started/supported-account-types) |
| Spark | Gmail; iCloud; Yahoo; Exchange; Outlook; Kerio Connect; generic IMAP/EWS. Separate documentation covers AOL/Verizon and Proton via Bridge. Proton support is for Mac/Windows desktop, requires a paid Proton plan, and does not extend to Spark mobile. | [Accounts](https://sparkmailapp.com/help/add-manage-accounts/connect-to-your-email-account-in-spark), [AOL/Verizon](https://sparkmailapp.com/help/troubleshooting/can-t-connect-to-an-aol-account), [Proton](https://sparkmailapp.com/blog/proton-mail-spark-mac-windows) |
| eM Client | Gmail/Google Workspace; Microsoft 365; Exchange; Outlook.com/MSN/Live/Hotmail; iCloud; Yahoo; AOL; GMX; IceWarp; SmarterMail; MDaemon; Imageway; Mailfence; Kerio; SOGo; Variomedia. Also supports generic IMAP/POP3. | [Service list](https://www.emclient.com/email-services), [FAQ](https://www.emclient.com/faq-getting-started) |
| Mailbird | Gmail/Google Workspace; Outlook.com/Hotmail/Live; Microsoft 365/Exchange Online; Yahoo; AOL; iCloud. These are the names in its provider-specific IMAP setup guide, which also includes generic setup. | [Provider setup](https://support.getmailbird.com/hc/en-us/articles/39932264536087-How-to-enable-IMAP-for-your-email-account-in-Mailbird) |
| Mailspring | Gmail/Google Workspace; Microsoft 365/Office 365; Outlook.com/Hotmail/Live/MSN; Yahoo; iCloud; Fastmail; GMX; Yandex; Proton via Bridge. Also names automatic settings for AOL, Zoho, mail.com, Posteo, 163.com, and QQ. Supports generic IMAP/SMTP, but not POP3 or native Exchange protocols. | [Supported providers](https://www.getmailspring.com/docs/supported-email-providers) |
| Thunderbird | Dedicated documentation covers Gmail; Microsoft 365/Office 365/Outlook.com/Hotmail/Live; Yahoo; AOL; AT&T. These are examples, not its complete provider list: Thunderbird uses a large configuration database and manual setup. | [Google](https://support.mozilla.org/en-US/kb/thunderbird-and-gmail), [Microsoft](https://support.mozilla.org/en-US/kb/thunderbird-and-hotmail), [Yahoo/AOL/AT&T](https://support.mozilla.org/en-US/kb/thunderbird-and-yahoo), [automatic setup](https://support.mozilla.org/en-US/kb/automatic-account-configuration) |
| Superhuman Mail | Its eligibility page specifies Google-hosted and Microsoft 365-hosted accounts and excludes on-premises Exchange. Its account-management page also refers to personal Outlook accounts; confirm exact consumer-account eligibility before relying on that wording. No generic IMAP-provider support is established by these pages. | [Eligibility](https://help.superhuman.com/hc/en-us/articles/44999410406029-Troubleshooting-Login-Issues), [account management](https://help.superhuman.com/hc/en-us/articles/46005777934733-Managing-Accounts) |

Recheck external requirements before implementation and release. In particular, Microsoft-managed consent policies, provider authentication programs, and subscription eligibility can change independently of Overlook's code.

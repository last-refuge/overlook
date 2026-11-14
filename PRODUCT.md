# Product

<!-- impeccable:product-schema 1 -->

## Platform

web

## Users

People on Linux desktops who want a beautiful, considered mail client, and who know the three-pane, Outlook-style workspace. The first person through the door is setting up their own accounts on their own computer: often more than one account, often from several services.

## Product Purpose

Overlook is a calm, Outlook-familiar native mail client for Linux, made by Last Refuge. Its promise is "a clear view of what matters". Success means the person's accounts are connected, mail is readable even offline, and daily mail feels quieter and more legible than in the clients they left.

## Positioning

A native Linux desktop client that pairs New Outlook's practical familiarity with Last Refuge's quiet alpine identity. It handles each major service its own way, instead of leaving people to type server settings: Google sign-in for Gmail; Microsoft sign-in; guided app-password setup for iCloud, Yahoo, AOL and Fastmail; Proton through Bridge with an explicit certificate trust step; and any IMAP or POP3 server. Mail is kept on the device for offline use.

## Operating Context

- **Environment:** Linux desktops, mainly Wayland. The author uses Omarchy with Hyprland. Pointer and keyboard, at desktop window sizes.
- **Themes:** Light, Dark, Gray, Blue, White, Alexander and Pink, or follow the system (Light or Dark).
- **First run:** shows the no-accounts welcome screen. From there the account setup dialog takes over: browser sign-in for Google and Microsoft, a guided dialog for the app-password services.

## Capabilities and Constraints

- **Implementation:** a native Rust application built with GPUI and gpui-kit. There is no HTML or CSS in the interface. The schema has no desktop value, so "web" above stands for a desktop-class pointer-and-keyboard surface. Web-only tooling (DOM detectors, live browser editing) does not apply.
- **Mail:** received HTML is sanitized and rendered by isolated Chromium. The app chrome is native.
- **Providers:**
  - Gmail through the Gmail API with Google sign-in.
  - Microsoft (Outlook.com, Hotmail, Live, Microsoft 365) through IMAP/POP3/SMTP with Microsoft sign-in.
  - iCloud, Yahoo, AOL and Fastmail through IMAP (or POP3 where offered) with app passwords.
  - Proton Mail through Proton Mail Bridge.
  - Any IMAP or POP3/SMTP server.
  - Fastmail calendars over CalDAV.
- **Credentials:** passwords and sign-ins live in the system keyring.
- **Qualification:** Gmail is qualified end to end. Microsoft, iCloud, Yahoo, Fastmail and Proton work with real accounts. Other services and Microsoft 365 work accounts are beta.
- **Security record:** Google's CASA assessment passed in November 2026, with Google's final letter pending. Never weaken certificate verification for remote servers. Never let people skip trusting a local server's certificate.

## Brand Commitments

The brand system is `llm-docs/overlook-brand.md`, with its assets in `branding/`. It sets these:

- **Name and endorsement:** the product is **Overlook**. "by Last Refuge" appears as an endorsement on the welcome screen and in the About window.
- **Lines:** "A clear view of what matters." and, for a first welcome, "A calmer place for your mail."
- **Voice:** plain language with quiet confidence. Navigation, setup and errors are literal. No jokes about overlooking or losing mail, and no exaggerated security promises.
- **Mark:** the horizon mark, which keeps its geometry.
- **Type:** Source Serif 4 Display for editorial and welcome moments, Inter for the working interface.
- **Colour:** the brand palette (Midnight, Alpine, Pine, Mist, Ivory, Paper, and Beacon amber as a small signature), and the native colorways in `crates/overlook-app/src/theme.rs`.
- **Imagery:** the alpine-dawn artwork belongs on onboarding, landing and launch surfaces, never behind working mail.

## Evidence on Hand

- **Overlook artwork** in `branding/assets/`: mark, lockups, wordmark, app icon, symbolic icon, and the alpine-dawn hero (`overlook-alpine-dawn.png`).
- **Icons:** the custom colour action icons in `branding/icons/`.
- **Fonts:** in `branding/fonts/`.
- **Screenshots:** in `docs/screenshots/`.
- **Provider logos** (confirmed 13 November 2026):
  - Official sign-in logos from Google's and Microsoft's branding kits.
  - Simple Icons for iCloud, AOL and Proton.
  - Overlook-made icons for Yahoo, Fastmail, IMAP and POP3, until the user replaces them.
- **Absent:** no testimonials, customers, benchmarks, pricing or download counts. Don't invent them.

## Product Principles

- **Calm before busy:** mail content outweighs chrome, and branding never competes with the work.
- **Familiar, then better:** keep the Outlook-style workspace people know, and refine it rather than reinvent it.
- **Literal and truthful:** reassure only about what is durably true, and say plainly what each service needs.
- **Security without bravado:** credentials in the keyring, verified connections, explicit trust where trust is a choice.

## Accessibility & Inclusion

- Text contrast of at least 4.5:1, and at least 3:1 for focus and control boundaries. The theme palettes are tested for this in `theme.rs`.
- Keyboard navigation with visible focus, honouring text scaling and reduced motion.
- Fall back to Noto Sans or the platform sans-serif for scripts the bundled fonts lack.

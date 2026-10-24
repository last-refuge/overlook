# Overlook Brand System

Overlook is a thoughtfully designed mail client for Linux, made by Last Refuge. Its identity translates an alpine overlook into a product promise: **a clear view of what matters**. The brand should feel composed, capable, warm, and spacious, with the practical familiarity of New Outlook and the independent character of Last Refuge.

This is the selected brand direction for implementation. Explore the [visual brand book](../branding/index.html) and use the [asset guide](overlook-brand-assets.md) to find the reusable artwork, local fonts, licenses, theme tokens, and build sources in `branding/`.

## Relationship to Last Refuge

The parent identity at [lastrefuge.ai](https://lastrefuge.ai/) uses a moonlit alpine setting, an illuminated refuge, near-black surfaces, warm ivory serif headlines, and amber accents. The site was reviewed visually on 12 October 2026. Its observed text colors include ivory `#F0EBDD` and amber `#FF9D32`; the remaining Overlook colors below are purpose-designed extensions, not asserted parent brand standards.

Overlook inherits the quiet landscape, restrained warmth, editorial typography, and care for open platforms. Its own territory is perspective: open sky, water, a distant horizon, and a readable workspace. Use **Overlook** as the product name, with **by Last Refuge** as an endorsement on the website, welcome screen, About window, and launch materials. The parent name should not compete with the product name in daily mailbox navigation.

An overlook is a place to pause and see the landscape clearly. Always reinforce that meaning through the horizon mark and clarity-focused copy. Avoid jokes about overlooking, ignoring, or losing messages.

## Position and voice

- **Primary line:** A clear view of what matters.
- **Supporting line:** A calmer place for your mail.
- **Product descriptor:** Thoughtfully made for Linux.
- **Short description:** Overlook is a beautiful, considered mail client for Linux. A familiar workspace for reading, writing, and keeping your day in view.
- **Parent endorsement:** Overlook by Last Refuge.

The brand speaks plainly and with quiet confidence. Marketing may evoke place and perspective. Navigation, errors, synchronization, and settings use literal language. Avoid productivity bravado, wilderness puns in commands, exaggerated security promises, and claims that the planned product already ships.

| Situation | Product language |
|---|---|
| First welcome | A calmer place for your mail. |
| Account setup | Connect your Gmail account. |
| Offline | You’re offline. Downloaded mail is still available. |
| Local draft saved | Saved on this device. |
| Queued outgoing message | Waiting for connection. We’ll send this when you’re back online. |
| Synchronizing | Syncing your changes… |
| Successful synchronization | Up to date. |
| Uncached attachment | This attachment hasn’t been downloaded. Connect to open it. |
| Reauthentication | Sign in again to continue syncing. Your local drafts are safe. |
| Empty inbox | Your inbox is clear. |

Use reassurance only when supported by durable state: never say a draft is safe before its transaction succeeds, or that mail was sent before provider acceptance. Keep uncertain delivery distinct from failure. Offline support follows the detailed product plan; this kit does not implement a mail engine.

## Logo system

The **horizon mark** combines a circular O with a low angular ridgeline and a small beacon. The open upper field creates a sense of perspective; the beacon connects the product to the warmth in Last Refuge’s imagery. A serif wordmark balances the geometric symbol with a human editorial voice.

- **Primary signature:** horizontal mark and wordmark, provided for light and dark backgrounds. Use the dark variant on midnight or a darkened image.
- **Wordmark:** the standalone Overlook name for large editorial applications and constrained compositions where the mark already appears nearby.
- **App icon:** ivory horizon mark and amber beacon on a midnight rounded square. Supplied as SVG and PNG at 16, 24, 32, 48, 64, 128, 256, and 512 pixels.
- **Symbolic icon:** a single-color aperture and ridge without the beacon. Use for small toolbar/system contexts. It uses `currentColor`; consumers may apply the foreground token.
- **Monochrome:** ink and ivory marks, plus an ink signature. No gradient or photograph is required for recognition.

Preserve the supplied geometry and wordmark proportions. The SVG wordmarks are outlined paths and need no installed font. Keep clear space of at least one quarter of the mark width around a standalone mark or signature. Recommended minimum digital signature width is 160 pixels; use the mark alone below that. Use the simplified artwork for 16–24 pixels. The PNG app icons at those sizes already omit the beacon.

Do not stretch, rotate, add shadows to the mark itself, recolor individual ridge segments, place the signature over busy image detail, or replace the wordmark with ordinary typed text in final marketing artwork. A soft shadow around the app tile in a presentation is acceptable; the shipped icon is flat.

## Color

| Color | Hex | Role |
|---|---|---|
| Midnight | `#101C24` | Brand anchor, dark canvas, app icon |
| Alpine | `#365F73` | Supporting slate in the brand identity |
| Pine | `#536E63` | Secondary illustration and supporting natural tone |
| Mist | `#C6D5D5` | Atmospheric backgrounds and quiet supporting graphics |
| Ivory | `#F0EBDD` | Dark-mode text, wordmark, editorial surfaces |
| Paper | `#FAF8F2` | Brand-book and marketing reading surfaces |
| Beacon | `#FF9D32` | Small signature accents inherited from Last Refuge |

Keep the interface predominantly neutral: approximately 85% surfaces and text, 10% slate and selected states, and at most 5% warm/secondary accents as a composition guide, not a runtime constraint. Amber is a signature, not the default color of all buttons. Never use amber body text on a pale background.

**The app offers Light, Dark, Gray, Blue, White, Alexander, and Pink.** Follow system selects Light or Dark. Light uses the Soft stone palette approved on 23 October 2026: stone reading and compose surfaces (`#E1E2DC`), a slightly darker frame (`#CDD1C9`) and sidebar (`#C4CBC1`), charcoal text (`#29332E`), clear secondary text (`#4B564D`), pine actions (`#3F6654`) and selections (`#BDCDBF`), and restrained amber details (`#92581D`). This is a design choice informed by reading/comfort research, not a claim that particular RGB values prevent eye strain.

The additional colorways use light rendering. Alexander combines dark blue navigation, white surfaces and gray highlights/chips, with a golden-orange email edge accent based on the author's reference swatch. Retired Gray/White preferences resolve to Gray. Those additional light colorways retain their white message center; Dark retains its dark surface. Light also supplies stone/charcoal defaults to unstyled HTML bodies; sender-authored CSS and legacy body color attributes take precedence. These display defaults never change stored messages or outgoing formatting. All themes retain the same icon, layout, hierarchy, typography, and status colors; custom colors and downloadable themes are not supported.

The current Light/Dark semantic values are in [tokens.json](../branding/tokens.json) and [tokens.css](../branding/tokens.css), with their generator in `branding/source/build_assets.py`; the native palettes live in `crates/overlook-app/src/theme.rs` and are tested for text contrast and component-token consistency. These are design tokens for the Rust/native implementation, not a decision to use web UI technology. Light's body and secondary text measure approximately 10.0:1 and 5.9:1 against the reading surface; secondary text on the sidebar measures 4.6:1. Dark uses blue-black surfaces, ivory text, pale slate actions, and a darker slate selection fill. Focus, success, warning, and error have independent accessible colors. Earlier static previews and the packaged brand archive depict the original Light palette; the source tokens and native application are authoritative for Soft stone.

The included contrast report validates 22 key foreground/background pairs. Normal text pairs meet at least 4.5:1, and focus/control boundaries at least 3:1 for the tested backgrounds. Subtle decorative dividers are not the sole identifier for interactive controls. These checks do not replace full accessibility review of the native UI, actual message content, overlays, or focus behavior.

## Typography

**Source Serif 4 Display Regular** is the editorial voice: the outlined wordmark, marketing headlines, welcome moments, and large brand statements. It extends the serif character observed on Last Refuge’s website. Use optical/display styling at large sizes; keep it out of dense mailbox rows.

**Inter** is the product voice: navigation, message lists, command bars, forms, settings, and default plain-text reading. Use Regular for body copy, Medium for labels, and Semibold for unread messages and stronger hierarchy. Avoid low-contrast light weights in working UI.

The brand kit bundles font files and original license texts. Official references: [Source Serif](https://github.com/adobe-fonts/source-serif) and [Inter](https://rsms.me/inter/). Bundle fonts with the app so appearance does not depend on internet access; fall back to Noto Sans or the platform sans-serif for unsupported scripts. The supplied Inter TTF is suitable for native font loading. Honor user text scaling and localization.

Suggested native starting sizes: 14–15 px for message rows and controls, 16 px for reading and composition, 12–13 px for secondary metadata, and 24–28 px for screen titles. Density controls change spacing first. The visual prototype is illustrative; shipping native controls must be checked at actual device scale.

## Imagery and composition

The hero image is original generated brand artwork: a misty alpine lake, layered ridges, expansive sky, and one small point of amber light. It belongs to Last Refuge’s visual world without reusing its website hero or parent logo. The generated scene is not a documented location.

Use photography-style imagery in launch materials, landing pages, and onboarding. Favor low horizons, atmospheric depth, believable natural textures, dark blue/slate light, restrained evergreen, and generous space for text. The full original and generation prompt are saved in the kit.

Avoid bright neon, fantasy mountains, oversized moons, excessive orange sunsets, rugged camping insignia, distressed textures, and landscapes behind working mail. Introduce warmth with proportion and light rather than a permanent orange wash. A dark overlay may be used beneath text; never rely on the photograph alone to ensure legibility.

## Product application

Preserve the established spacious three-pane Outlook-inspired design: account/folder navigation, message list, generous reading pane, clear search, and restrained command bar. Use a small app icon and serif product name in the header; use Inter throughout the working interface. Foreground email content should visually outweigh branding.

Use consistent restrained line icons for actions, 6–8 px control corners, 10–12 px floating-panel corners, subtle structural borders, and modest shadows only for elevation. Use brief 120–180 ms feedback transitions; respect reduced-motion preferences. Selected and unread states need structure, indicators, and typography as well as color.

The [mail application preview](../branding/mail-preview.html) applies this identity to the earlier interaction prototype. It uses fictional sample mail and local in-memory actions; it does not connect to Gmail, deliver messages, or provide durable draft storage. The original refined prototype remains available separately.

Branded appearance applies to app chrome and default plain-text content. Preserve sender formatting in HTML messages, offer an original-color fallback, and never alter outgoing email formatting merely because the user switches themes.

## Implementation checklist

- Adopt the horizon icon and endorsed name in the welcome screen, About window, desktop launcher, app metadata, and launch materials.
- Map semantic tokens to native styles; reserve the bitmap landscape for appropriate brand surfaces.
- Bundle fonts and their licenses, retaining platform and script fallbacks.
- Keep Light/Dark settings persistent with instant switching and system-following support.
- Review reading, compose, search, account setup, offline, sync failure, uncertain delivery, and empty states in both themes.
- Verify keyboard navigation, visible focus, screen readers, scaling, reduced motion, and icon recognition at real sizes.
- Use the offline status wording only when it accurately describes the synchronization engine.

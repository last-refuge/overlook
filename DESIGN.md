---
name: Overlook
description: A clear view of what matters. A calm, Outlook-familiar native mail client for Linux, by Last Refuge.
colors:
  midnight: "#101C24"
  alpine-slate: "#365F73"
  pine: "#536E63"
  mist: "#C6D5D5"
  ivory: "#F0EBDD"
  paper: "#FAF8F2"
  beacon: "#FF9D32"
  stone-frame: "#CDD1C9"
  soft-stone: "#E1E2DC"
  stone-sidebar: "#C4CBC1"
  pale-green-list: "#F3F7F1"
  unread-green: "#E3EBE0"
  reading-white: "#FFFFFF"
  charcoal: "#29332E"
  moss-gray: "#4B564D"
  stone-divider: "#A7B0A5"
  control-edge: "#657267"
  stone-hover: "#D5DAD1"
  pine-action: "#3F6654"
  sage-selection: "#BDCDBF"
  deep-amber: "#92581D"
  light-success: "#386449"
  light-warning: "#805015"
  light-error: "#A13E37"
  night-frame: "#101A21"
  night-surface: "#17242C"
  night-sidebar: "#121F27"
  night-muted: "#A8B7BA"
  night-divider: "#30424B"
  night-control-edge: "#71878D"
  night-hover: "#23353F"
  night-chip: "#22323B"
  glacier-action: "#A9CDD9"
  night-on-action: "#10232D"
  night-selection: "#28404C"
  night-selection-text: "#DCECEF"
  lantern-focus: "#FFBA6B"
  dark-success: "#A8CEB1"
  dark-warning: "#F1BE78"
  dark-error: "#FFB0A6"
typography:
  display:
    fontFamily: "Source Serif 4 Display, serif"
    fontSize: "48px"
    fontWeight: 400
    lineHeight: "56px"
  wordmark:
    fontFamily: "Source Serif 4 Display, serif"
    fontSize: "22px"
    fontWeight: 400
    lineHeight: "30px"
  headline:
    fontFamily: "Inter, Noto Sans, sans-serif"
    fontSize: "18px"
    fontWeight: 600
    lineHeight: "24px"
  title:
    fontFamily: "Inter, Noto Sans, sans-serif"
    fontSize: "16px"
    fontWeight: 600
    lineHeight: "22px"
  lead:
    fontFamily: "Inter, Noto Sans, sans-serif"
    fontSize: "15px"
    fontWeight: 400
    lineHeight: "22px"
  control:
    fontFamily: "Inter, Noto Sans, sans-serif"
    fontSize: "14px"
    fontWeight: 400
  body:
    fontFamily: "Inter, Noto Sans, sans-serif"
    fontSize: "13px"
    fontWeight: 400
    lineHeight: "19px"
  label:
    fontFamily: "Inter, Noto Sans, sans-serif"
    fontSize: "12px"
    fontWeight: 500
    lineHeight: "16px"
  caption:
    fontFamily: "Inter, Noto Sans, sans-serif"
    fontSize: "11px"
    fontWeight: 400
rounded:
  control: "6px"
  track: "8px"
  panel-sm: "10px"
  panel: "12px"
  full: "9999px"
spacing:
  hair: "2px"
  xxs: "4px"
  xs: "6px"
  sm: "8px"
  sm-plus: "10px"
  md: "12px"
  lg: "16px"
  xl: "20px"
  xxl: "32px"
components:
  button-primary:
    backgroundColor: "{colors.pine-action}"
    textColor: "{colors.reading-white}"
    rounded: "{rounded.control}"
    typography: "{typography.control}"
  button-primary-dark:
    backgroundColor: "{colors.glacier-action}"
    textColor: "{colors.night-on-action}"
    rounded: "{rounded.control}"
  button-secondary:
    backgroundColor: "{colors.stone-hover}"
    textColor: "{colors.charcoal}"
    rounded: "{rounded.control}"
    typography: "{typography.control}"
  input:
    textColor: "{colors.charcoal}"
    rounded: "{rounded.control}"
    typography: "{typography.control}"
  segmented-track:
    backgroundColor: "{colors.stone-sidebar}"
    rounded: "{rounded.track}"
    padding: "3px"
  segment-selected:
    backgroundColor: "{colors.soft-stone}"
    textColor: "{colors.charcoal}"
    rounded: "{rounded.control}"
    padding: "4px 10px"
    typography: "{typography.body}"
  segment-unselected:
    textColor: "{colors.moss-gray}"
    rounded: "{rounded.control}"
    padding: "4px 10px"
  dialog-panel:
    backgroundColor: "{colors.soft-stone}"
    textColor: "{colors.charcoal}"
    rounded: "{rounded.panel}"
  choice-card-featured:
    backgroundColor: "{colors.reading-white}"
    textColor: "{colors.charcoal}"
    rounded: "{rounded.panel}"
    padding: "30px 20px"
    height: "112px"
  choice-card-featured-dark:
    backgroundColor: "{colors.night-chip}"
    textColor: "{colors.ivory}"
    rounded: "{rounded.panel}"
  choice-card-compact:
    backgroundColor: "{colors.reading-white}"
    textColor: "{colors.charcoal}"
    rounded: "{rounded.panel}"
    padding: "14.5px 14px"
    height: "68px"
  choice-card-quiet:
    textColor: "{colors.charcoal}"
    rounded: "{rounded.panel}"
    padding: "9px 12px"
    height: "56px"
  logo-plate:
    backgroundColor: "{colors.reading-white}"
    size: "52px"
  beacon-dot:
    backgroundColor: "{colors.beacon}"
    rounded: "{rounded.full}"
    size: "9px"
  welcome-banner:
    backgroundColor: "{colors.midnight}"
    textColor: "{colors.ivory}"
    typography: "{typography.display}"
    height: "220px"
---

# Design System: Overlook

## Overview

**Creative North Star: "A Clear View of What Matters"**

Overlook translates an alpine overlook into a working mail client: a place to pause and see the landscape clearly. The system is two registers kept firmly apart. The working interface (mail lists, reading pane, settings, dialogs, the welcome's choices) is neutral, spacious and literal: Inter on theme surfaces, pine or glacier-blue for action, flat cards on tonal grounds. The brand register (the horizon mark, the serif wordmark, the alpine-dawn photograph, a single amber beacon) appears only at edges of the experience: title-bar lockup, welcome, About, launch materials. Mail content always outweighs branding.

Density is Outlook-familiar: a three-pane workspace, 12 to 13 px Inter for metadata and lists, generous reading surfaces that stay white in every light colorway. Seven colorways (Light "Soft stone", Dark, Gray, Blue, White, Alexander, Pink, plus Follow system, which can adopt an Omarchy desktop palette) all share one icon set, one layout, one type hierarchy and one set of status colors; only color changes. The palettes are centralized in `crates/overlook-app/src/theme.rs` and tested for contrast; no screen invents its own palette.

Confirmed rejections, from the brand system: no bright neon, fantasy mountains, oversized moons, orange sunset washes or camping insignia; no landscape behind working mail; no amber body text on pale grounds; no wilderness puns or jokes about overlooking mail.

**Key Characteristics:**
- Neutral-first: roughly 85% surfaces and text, 10% slate/pine and selection, at most 5% warm accent.
- Two type voices: Source Serif 4 Display for brand moments, Inter for everything that works.
- Flat tonal layering at rest; shadows only for floating elevation.
- The beacon: one small amber light, never a fill colour for controls.
- Every interactive element has a visible keyboard focus state in the theme's focus colour.

## Colors

A cool, mineral palette (stone, pine, slate, blue-black) warmed by ivory and one point of amber light.

### Primary
- **Pine Action** (#3F6654): Light's action colour: primary buttons, links, active-tab text, hover edges on choice cards, the primary stroke of Overlook's colour icons. White text on it.
- **Glacier Action** (#A9CDD9): Dark's action colour, paired with Night On-Action (#10232D) text.

Other colorways supply their own action: Gray `#374151`, Blue `#245A96`, White `#374151`, Alexander `#1F57A5`, Pink `#963D68` (see `theme.rs`).

### Secondary
- **Alpine Slate** (#365F73): the brand identity's supporting slate; marketing, brand book and illustration. In the native app it survives only as the remap key of the component-library template, not as a painted colour.
- **Pine** (#536E63) and **Mist** (#C6D5D5): illustration and atmospheric supporting tones in brand materials.

### Tertiary
- **Beacon** (#FF9D32): the signature amber inherited from Last Refuge. In the app it appears in the horizon mark's beacon and the welcome choice-card beacon dot, and as Dark's brand accent. Alexander uses a golden-orange variant (`#FF9F0A`) for its message edge accent.
- **Deep Amber** (#92581D): Light's restrained amber; serves as Light's focus ring and brand accent, and as the secondary tone in Overlook's colour icons.
- **Lantern Focus** (#FFBA6B): Dark's focus ring.

### Neutral
- **Midnight** (#101C24): brand anchor; app-icon tile, title-bar brand tile, welcome banner base and its veil.
- **Ivory** (#F0EBDD): Dark's foreground text, and the colour of words on the welcome photograph in every theme.
- **Paper** (#FAF8F2): brand-book and marketing reading surface only.
- **Soft Stone** (#E1E2DC): Light's surface; title bar, dialogs, the welcome ground.
- **Stone Frame** (#CDD1C9) / **Stone Sidebar** (#C4CBC1): Light's outer frame and folder navigation, slightly darker than the surface.
- **Pale Green List** (#F3F7F1) / **Unread Green** (#E3EBE0): Light's message list and its unread rows.
- **Reading White** (#FFFFFF): the message centre in every light colorway; the card fill for welcome choices in light themes; the plate under third-party logos in every theme.
- **Charcoal** (#29332E) / **Moss Gray** (#4B564D): Light's body and secondary text (about 10.0:1 and 5.9:1 against the reading surface).
- **Stone Divider** (#A7B0A5), **Control Edge** (#657267), **Stone Hover** (#D5DAD1), **Sage Selection** (#BDCDBF): Light's structural lines, input borders, hover/chip fill and selection fill.
- Dark's neutrals: Night Frame (#101A21), Night Surface (#17242C), Night Sidebar (#121F27), Night Muted (#A8B7BA), Night Divider (#30424B), Night Control Edge (#71878D), Night Hover (#23353F), Night Chip (#22323B), Night Selection (#28404C) with Night Selection Text (#DCECEF).
- Status: success, warning and error each have an independent accessible value per mode (Light #386449 / #805015 / #A13E37; Dark #A8CEB1 / #F1BE78 / #FFB0A6).

### Named Rules
**The Beacon Rule.** Beacon amber is a small light, not a fill. It may appear as the mark's beacon or as a dot of a few pixels that lights on hover or focus. Never use it as a button fill, body text, or a wash.

**The One Palette Source Rule.** Every app colour comes from `Palette` in `theme.rs`, or is derived from it with a stated mix (as the welcome's card tones are). Brand constants (Midnight, Ivory, Beacon) are painted only on brand material: the tile, the mark, the photograph.

**The White Paper Rule.** Every light colorway keeps a white message centre; email content renders on white paper with `#20242B` ink regardless of the shell theme. Sender colours always win.

## Typography

**Display Font:** Source Serif 4 Display (with serif fallback), Regular only
**Body Font:** Inter (with Noto Sans, then the platform sans-serif)

**Character:** An editorial serif borrowed from Last Refuge's headlines, set against a clear, workmanlike grotesque. The serif speaks for the brand; Inter does the work.

### Hierarchy
- **Display** (400, 48px, 56px line): the welcome headline on the photograph, ivory. Max width 640px.
- **Wordmark** (400, 22px, 30px line): the typed "Overlook" beside the brand tile in the title bar.
- **Headline** (Inter 600, 18px, 24px line): section heads such as "Connect your mail".
- **Title** (Inter 600, 16px, 22px line): featured choice-card names; reading-pane subjects at similar weight.
- **Lead** (Inter 400, 15px, 22px line): the line beneath the display headline; ivory at 84% opacity on the banner. Max width 520px.
- **Control** (Inter 400, 14px): gpui-kit controls' default size (inputs, buttons, menus).
- **Body** (Inter 400, 13px, 19px line): the window's base text size; secondary descriptions and segmented controls.
- **Label** (Inter 500, 12px, 16px line): small headings for groups ("Another provider, or your own server"), metadata, the keyring line. The most-used size in the codebase.
- **Caption** (Inter 400, 11px): badges, counts, dense metadata.

Weights in use: Regular for reading, Medium for labels and quiet choices, Semibold for unread messages, selected segments and stronger hierarchy. No light weights in working UI.

### Named Rules
**The Two Voices Rule.** Source Serif 4 Display appears only in brand moments (wordmark, welcome, About, marketing), never in mailbox rows, forms or dialogs. Inter carries everything else.

**The Sentence Case Rule.** Headings and labels are plain sentence case in Inter; there are no uppercase labels above headings. The one native exception is the calendar's weekday column headers (11px uppercase day abbreviations), which are data labels, not decoration.

## Layout

The working window is the Outlook-style three-pane layout: folder navigation, message list, generous reading pane, with a title bar of 52px (density-dependent in the shell) carrying the lockup and commands.

Brand surfaces use a centred content column: the welcome screen caps its column at 1040px with 32px side gutters, and the banner's words align to that column's left edge. A full-bleed banner takes about a third of the window height, clamped between 220px and 300px, so a short window still shows every choice. Choice grids use flex-wrap with a basis per tier (260px featured, 200px compact) so columns fall from 3 or 4 to fewer at narrow widths (about 900px) without shrinking text.

Spacing is a 2/4-based rhythm with 8px the most common step, then 10, 6 and 12. Group rhythm on the welcome: 36px above the section, 20px heading-to-grid, 16px between featured cards, 12px between compact cards, 32px before a quieter tier, 28px before a closing note, 44px bottom.

## Elevation & Depth

Flat by default, layered by tone. Panes and cards separate by stepping between frame, surface, list and reading-white fills, plus 1px structural borders. Shadows appear only on things that float above the window.

### Shadow Vocabulary
- **Floating panel** (gpui-kit `shadow_lg`): dialogs, popovers, the command palette, history and folder management panels.
- **Menu / dragged item** (`shadow_md`): toolbar menus and a calendar event while dragged.
- **Raised segment** (`shadow_sm`): the selected option of a segmented control.
- **Focus ring** (`0 0 0 2px` in the theme's focus colour, no blur): keyboard focus on custom controls such as choice cards. A ring, not elevation.

### Named Rules
**The Flat-At-Rest Rule.** Cards on a page carry no shadow; they lift by fill and a 1px edge. If a ground and its cards are too close in luminance (the white-surface colorways), step the ground toward the frame or ink rather than adding a shadow.

**The Unshadowed Mark Rule.** The horizon mark and app icon are never given shadows in the product.

## Shapes

Softly rounded rectangles, sized to their role: 6px for controls and segments (the component library's base radius), 8px for tracks and small containers, 10px for the library's large radius, 12px for dialogs, floating panels and welcome choice cards. Plates and tiles round in proportion to their size (about 27% for logo plates; 28/128 of the tile for the brand tile, following the app icon). Dots, avatars and the beacon are full circles. Borders are 1px hairlines in the divider colour or a tone between divider and fill.

## Components

### Buttons
Restrained and literal.
- **Shape:** gently rounded (6px).
- **Primary:** action fill with on-action text (pine and white in Light, glacier and night ink in Dark); hover and active shade the action by 12% and 24%.
- **Secondary:** hover-tone fill with foreground text.
- **Danger:** error fill.
- **Focus:** the theme's focus colour as ring.

### Segmented control
- **Track:** sidebar fill, 1px divider edge, 3px inset, 8px radius, 2px gap.
- **Selected:** surface fill, foreground semibold text, control-edge border, `shadow_sm`.
- **Unselected:** flat, muted text; hover takes the hover fill and foreground text.

### Dialogs / floating panels
Surface fill, 1px divider border, 12px radius, `shadow_lg`, over the theme overlay.

### Choice cards (welcome)
The services a person can connect, each a focusable button with an accessible name ("Name: detail") and its service's own mark. Three tiers share one component.
- **Featured** (112px min height, 30px/20px padding, 52px plate, 16px name / 13px detail, trailing arrow): the most used services.
- **Compact** (68px, 14.5px/14px padding, 36px plate, 14px / 12px).
- **Quiet** (56px, own width, 32px plate, 13px medium name): no fill or edge until hovered or focused; for generic IMAP and POP3.
- **Fill:** reading white in light themes, chip in dark. **Edge:** divider mixed 35% toward the card in light, divider in dark.
- **Hover:** fill mixed 30% toward selection, edge becomes the action colour, arrow turns to action. **Active:** 55% toward selection.
- **Keyboard focus:** edge and a 2px solid ring in the focus colour; a pointer click does not leave focus behind.
- Long details wrap to two lines instead of truncating; names truncate.

### Mark plates
- **Logo plate:** third-party marks sit unaltered on white, with a hairline edge in light themes and none in dark. The logo fills 58% of the plate with a per-mark optical scale.
- **Own plate:** Overlook's colour icons (68% of the plate) on the card mixed 50% toward selection.
- **Beacon:** a 7px amber light, ringed 1px by the hovered card's fill, top-right of the plate; lights on hover or keyboard focus only.

### Colour icons
Overlook's own 32-unit drawings (rounded strokes, soft fills, restrained two-tone accents) in `branding/icons/`. They are drawn in Light's tokens and retinted per theme and per contrast context (normal or on-action) by `assets.rs`. Lucide line icons are used for small glyphs such as arrows and the lock.

### Horizon banner (welcome)
The alpine-dawn photograph, cover-fit on a midnight base, with a midnight gradient from 82% opacity at the left to clear at 68% across so the words never rely on the photograph for legibility. Display headline and lead in ivory. A midnight veil at 60% lifts once over 1.4s with an exponential ease-out as the window opens; the words never wait for it.

### Title-bar lockup
The brand tile (midnight rounded square, ivory mark, amber beacon), typed wordmark, and on welcome and About only, "by Last Refuge" in 12px muted Inter.

## Do's and Don'ts

### Do:
- **Do** take every colour from `Palette` and test new foreground/background pairs for 4.5:1 (3:1 for focus and control boundaries), as `theme.rs` does.
- **Do** keep the message centre white in every light colorway and email on white paper.
- **Do** put a midnight shade under any words set on the photograph.
- **Do** give every custom interactive element a keyboard focus state in the theme's focus colour, and an accessible name.
- **Do** show third-party sign-in marks unaltered, on white plates, with provenance recorded in `branding/providers/README.md`.
- **Do** keep feedback transitions brief (120 to 180 ms); reserve longer motion for a single brand moment such as the welcome veil.

### Don't:
- **Don't** use Beacon amber (#FF9D32) as a control fill, body text or wash; it is a small light.
- **Don't** set amber body text on a pale background.
- **Don't** use Source Serif 4 Display in mailbox rows, forms or dialogs.
- **Don't** place the alpine landscape behind working mail; it belongs to onboarding, landing and launch surfaces.
- **Don't** add shadows to cards at rest or to the horizon mark.
- **Don't** invent screen-specific palettes or user-defined custom colours.

---
version: 1
slug: "crates-overlook-app-src-ui-mod-rs"
primary_target: "crates/overlook-app/src/ui/mod.rs"
related_targets: []
---

# Welcome screen (no accounts on this device)

**Scope and mode.** The whole window while the device has no accounts (`MailWindow::render_no_account`). Mode: Operate. The person's task is to connect their first account.

**Audience and job.** A person on Linux opening Overlook for the first time. They want their own mail in, from the service they already use. The proof is the real service logos and plain words about what each one needs. Constraints:
- Every theme, Light through Pink.
- Keyboard focus on every choice.
- Window widths from about 900 to 1700 px.
- Google's and Microsoft's sign-in logos used unaltered.

**Chosen direction.** "Horizon banner and grid", with Gmail, Microsoft and iCloud featured larger. The memorable moment is the dusk veil lifting off the lake when the window opens.

**Unresolved.** Yahoo, Fastmail, IMAP and POP3 use Overlook-made placeholder icons until the user supplies better ones.

## Direction contract

THESIS: The welcome is a view, not a form. The alpine-dawn horizon opens the window, and the services rest beneath it as a calm, weighted choice. It refuses the category default: a centred empty-state icon above a row of identical buttons.

OWN-WORLD: The Overlook brand world.
- **Banner:** the blue-hour lake photograph under a midnight veil, with an ivory Source Serif 4 Display headline.
- **Working area:** Inter text on theme surfaces (stone in Light, blue-black in Dark), and white logo plates carrying the real marks.
- **Accents:** pine for action. Beacon amber appears only as a 7 px dot that lights on the chosen plate.

STORY: The person sees a calm place and recognises their provider by its logo within a second. One click starts that service's own sign-in. IMAP and POP3 stay findable for everything else. A closing line says sign-ins are kept in the system keyring.

FIRST VIEWPORT:
- **Title bar:** the Overlook lockup with "by Last Refuge".
- **Banner:** full bleed, a third of the window's height within 220 to 300 px, so a short window still shows every choice. "A calmer place for your mail." sits at 48 px, left-aligned to the content column, with "Welcome to Overlook. Connect an account to bring your mail in." beneath it.
- **Column** (1040 px maximum), under "Connect your mail":
  - Three 112 px featured cards: Gmail, Microsoft, iCloud Mail.
  - Four 68 px cards: Yahoo, AOL, Fastmail, Proton.
  - A quieter row: IMAP and POP3 at their own width, with no fill or edge until hovered or focused.
  - The keyring line.
- **Primary action:** each card is the action.

FORM: Horizon banner and grid, second on the ordered list of seven; seed key 824e9e12. The signature interaction is the hovered or keyboard-focused card lighting its beacon. The motion is the veil lifting once, over 1.4 s with an exponential ease-out.

FINISH: unreviewed and undocumented is unfinished; this build ends with the finish review, the verdict, DESIGN.md, and every shipping raster carrying its provenance

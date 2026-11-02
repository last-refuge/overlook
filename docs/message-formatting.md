# Message rendering and isolation

## Browser message surface

Received bodies now use an isolated, live Chromium document inside the native GPUI
shell. Chromium performs the actual HTML/CSS layout and supplies bounded 512-CSS-pixel
PNG tiles, decoded off the UI thread. The native reading pane owns scrolling and
its draggable scrollbar: ordinary wheel/trackpad motion translates cached textures,
without waiting for a browser frame. Uncached regions are captured incrementally;
large jumps can briefly show an unpainted region while its tiles arrive. Twelve tiles
per view, one active decode and revision guards bound storage and reject stale work.
Same-message reflow retains painted tiles until their replacements arrive, rather
than blanking the body. Consent/theme changes clear previous pixels; non-inline
attachment downloads do not reload the browser document.
A 2x browser backing surface preserves sharp text on supported HiDPI displays.
GPUI forwards document-relative pointer, selection and keyboard input to the live DOM;
Chromium root scrollbars are hidden. This is not a
static screenshot substitute or an external browser window. Cards, rounded links,
background panels, stylesheet cascade, responsive rules, tables and typography
are rendered by the browser rather than approximated by native text layout.

`overlook-core::browser::prepare_document` starts from stored original HTML, not
from the native sanitizer's lossy presentation projection. It preserves safe
inline/head CSS and actual HTML/body root attributes, while removing active
markup, event handlers, forms, embedded frames, metadata, base URLs, external
stylesheet links and unsafe resource schemes. Original HTML in SQLite is never
rewritten. Authored layout containers remain intact. Before browser sanitization,
`thread_history::prepare` can add a reversible Message history disclosure around
a quoted tail whose whitespace-normalized text, images and link targets exactly
match an earlier message in the conversation. Gmail/cite/Thunderbird/Yahoo quote
containers and Outlook reply headers are recognized. Unmatched history, changed
inline notes/resources, short ambiguous quotes and forwarded content stay visible.
All original content remains inside the disclosure; outgoing quotes, printing,
raw mail and stored bodies are unchanged. Preparation runs off the UI thread with
generation cancellation, parsing limits and a 64-message / 8 MiB thread budget.
CID image sources are normalized, then resolved from downloaded local attachment
parts into data URLs. Unresolved CIDs cannot fetch through the browser.

Reader, authored composer preview, outgoing quote preview and outbox use this
surface. Outgoing quote serialization still deliberately uses the existing safe
CSS subset: its preview shows that serialized quote, not an untouched original.
Native composer editing remains native. Printing still uses the separate safe
printing projection; it is not yet guaranteed to match the new inline surface.

## Defense in depth

- Chromium's sandbox remains enabled. No `--no-sandbox`, debugging TCP port or
  ordinary user browser profile is used. Each view has a private pipe, incognito
  context and ephemeral, permission-restricted profile/home/cache directory.
- Scripts are disabled by the engine before mail is loaded. Source scripts and
  event handlers are also removed. Fixed trusted CDP controller expressions only
  measure layout, selection and links; untrusted email JavaScript never runs.
- A restrictive CSP is installed before authored markup. CSP is defense in depth,
  **not** the resource-consent gate.
- All browser requests are intercepted. Chromium requests are never continued.
  Without consent, external images, fonts, CSS backgrounds and other resources
  are blocked. With consent, only bounded anonymous HTTP(S) GET image/font/style
  fetching is eligible; no user cookies, authentication or referrer are forwarded.
  Private/local destinations are rejected by the actual connector resolver. Up to
  five anonymous redirects are followed with per-hop URL validation; private targets,
  credentials, unsafe schemes, loops and HTTPS downgrades remain blocked. Protocol-relative
  resource URLs are normalized to HTTPS; arbitrary relative paths remain unsupported.
  Two bounded fetching workers per view keep network waits off the input/render actor.
- Browser-side DNS/proxy restrictions additionally constrain accidental requests.
  Frames, document navigation, downloads and popups are isolated. External links
  are handed to the native shell only after real user interaction; live anchor
  targets are rewritten to inert links. This can affect href-value CSS selectors
  and internal fragment links.
- Chromium uses a short 0700 private runtime/profile directory in `/dev/shm`,
  also its child-only `TMPDIR`. It does not inherit a quota-exhausted `/tmp` or a
  deep project temp path; the latter can exceed Chromium's Unix socket limit.
  The launcher does not force shared-memory/font buffers onto `/tmp`.
- Four live browser views maximum, reserving slots for composer previews and
  prioritizing the latest message and reply source. Other expanded messages use
  their full native text part when present; HTML-only messages offer Read message
  to open the complete browser body. Process RSS is polled with a 2 GiB watchdog per
  view (not a hard cgroup memory limit). HTML, response bodies, queues and frames
  are bounded. Document display height is capped at 100,000 CSS pixels; the stored
  original is unchanged. Closing/collapsing a view cancels its worker, reaps subprocesses and
  removes its private profile. Optional failed or cancelled resources do not hide the document.
  Capture clips are padded to at least one physical pixel to avoid Chromium's
  subpixel-tail capture timeout; padding is clipped by the native document surface.
  On engine failure, available rendered tiles remain as an explicitly frozen preview
  with a privacy-safe error code and **Retry**. Raw engine diagnostics are never shown;
  retry preserves the prepared document, theme and resource-consent setting.
- Remote consent remains an explicit per-message choice or the global **Remote
  content** preference. The legacy persisted preference key remains compatible.

White is the fallback browser paper in every non-dark shell theme. Dark uses
Chromium dark-mode emulation; sender-authored colors need not match the shell
palette exactly. Fonts depend on installed system fonts and consent for authored
remote fonts. Animation/transition playback is suppressed. Full DOM accessibility
bridging is not implemented yet.

## Runtime and packaging

Chromium is required for message bodies. The Arch package depends on `chromium`.
Other installations must provide a compatible Chromium executable on PATH, or set
`OVERLOOK_BROWSER_BIN` to its file path. A missing engine or failed sandbox is an
explicit rendering error, never a silent switch back to lossy native rendering.
Do not disable the sandbox to work around an installation failure.

## Validation

All fixtures are synthetic. Never commit or export private mailbox bodies,
tracking URLs, authentication material or screenshots of private messages.

```sh
cargo test --workspace
cargo test -p overlook browser_ -- --include-ignored --test-threads=1
CARGO_TARGET_DIR="$PWD/target" cargo test --manifest-path vendor/gpui-base/Cargo.toml --lib --features test-support
cargo build -p overlook
git diff --check
```

Core browser tests cover inert markup, preserved stylesheet/table/root semantics,
CID normalization, CSS URL safety and the CSP envelope. Real Chromium tests cover
script suppression, live styles/geometry, selection, wheel input, link dispatch,
HiDPI dimensions, full-document tile coordinates/cache eviction, real consented image decoding,
redirect/private-target handling, slow-fetch responsiveness, request interception and
subprocess/profile cleanup. Visual
review at multiple widths/scales and light/dark modes remains necessary: passing
structural tests alone does not prove an Outlook/reference-perfect result.

## Native pipeline retained for other uses

The [native formatting contract](../vendor/gpui-base/HTML_FORMATTING.md) documents
native Markdown/editor and legacy safe presentation behavior. Native regressions
fixed during this work include paragraph state identity (independent HTML blocks
must not overwrite one another's retained layouts), terminal break-only line-box
semantics, and Unicode MIME alternatives. Full-frame Linux font tests cover those
fixes. The prior aggregate audit established cleaning/reader DOM stability, not
visual fidelity; it was never evidence that native text layout could reproduce
browser containers.

Quoted CID images still need a dedicated outgoing MIME attachment lifecycle:
keeping a CID URL or displaying a locally resolved image does not make it available
to recipients. No theme or display change mutates the stored original or outgoing
message content.

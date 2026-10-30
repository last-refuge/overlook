# Vendored crates

`gpui-base` is based on the crates.io release named in its `Cargo.toml` (Apache-2.0),
wired in through `[patch.crates-io]` in the root workspace. Overlook's native mail
formatting changes are in `src/text/format/html.rs`, `src/text/format/html/style.rs`,
`src/text/node.rs`, `src/text/inline.rs`, and `src/text/inline_flow.rs`.

They preserve hard breaks and HTML flow/paragraph distinctions, resolve bounded
inherited presentation, shape mixed-size text without code-chip decoration, preserve
list boundaries, and distinguish authored HTML table headers and footers. Markdown
keeps its own layout defaults. This is not a browser-equivalent HTML/CSS engine.

See [native capabilities and limits](gpui-base/HTML_FORMATTING.md) and the
[full formatting review](../docs/message-formatting.md).

The single-line `InputState` also has ghost text, which the code editor already had through its
language-server support: `set_ghost_text` / `ghost_text` (`src/input/base/state.rs`), `LineExtras` and the
`InputMode` hooks in `src/input/base/kind.rs`, and the new `InputEvent::AcceptSuggestion`. The text is
painted after the caret and is never part of the value; Tab emits the event (the field registers the
`IndentInline` action only while ghost text is showing, so Tab keeps moving focus otherwise) and leaves what
accepting means to the listener; any edit, caret movement, click, Enter, Escape or blur drops it. The
painter skips the covering fill when the caret ends a single-line field, because that field is not
necessarily drawn on the editor background. Overlook's composer uses it for recipient completion.

To upgrade: review and reapply these changes, run both `cargo test --workspace` and
`cargo test --manifest-path vendor/gpui-base/Cargo.toml --lib --features test-support`,
and visually review the synthetic email corpus. Do not remove the patch until upstream
provides equivalent parsing, layout, selection, and security-policy integration.

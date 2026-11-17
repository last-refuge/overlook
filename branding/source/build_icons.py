#!/usr/bin/env python3
"""Build Overlook's small-size, two-layer vector icons (original artwork).

The native renderer paints the fill and stroke groups separately at the control's
device-pixel size. Keep groups flat: assets.rs extracts them without an XML DOM.
"""
from pathlib import Path

ICONS = {
    "mail": (
        '<rect x="3" y="5" width="18" height="14" rx="2.5"/>',
        '<rect x="3" y="5" width="18" height="14" rx="2.5"/>'
        '<path d="m4 7 8 6 8-6"/>',
    ),
    "calendar": (
        '<path d="M3 10h18V7a2 2 0 0 0-2-2H5a2 2 0 0 0-2 2Z"/>',
        '<rect x="3" y="5" width="18" height="16" rx="2.5"/>'
        '<path d="M7 3v4m10-4v4M3 10h18m-13 5h3m3 0h2m-8 3h3"/>',
    ),
    "contacts": (
        '<circle cx="9" cy="8" r="3"/><path d="M3 20v-2a6 6 0 0 1 12 0v2Z"/>',
        '<circle cx="9" cy="8" r="3"/><path d="M3 20v-2a6 6 0 0 1 12 0v2H3Z'
        'm13-15a3 3 0 0 1 0 6m2 3a5 5 0 0 1 3 4v2"/>',
    ),
    "compose": (
        '<path d="m11 13 8-9 3 3-8 9-4 1Z"/>',
        '<path d="M9 5H5a2 2 0 0 0-2 2v12a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-5'
        'm-8-1 8-9 3 3-8 9-4 1 1-4Zm6-7 3 3"/>',
    ),
    "archive": (
        '<rect x="3" y="3" width="18" height="5" rx="1.5"/>',
        '<rect x="3" y="3" width="18" height="5" rx="1.5"/>'
        '<path d="M5 8v11a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8m-10 5h6"/>',
    ),
    "delete": (
        '<path d="m6 7 1 12a2 2 0 0 0 2 2h6a2 2 0 0 0 2-2l1-12Z"/>',
        '<path d="M4 7h16M9 7V3h6v4M6 7l1 12a2 2 0 0 0 2 2h6a2 2 0 0 0 2-2l1-12'
        'm-8 4v6m4-6v6"/>',
    ),
    "move": (
        '<path d="M3 9V6a2 2 0 0 1 2-2h4l3 3h7a2 2 0 0 1 2 2Z"/>',
        '<path d="M3 9V6a2 2 0 0 1 2-2h4l3 3h7a2 2 0 0 1 2 2v10a2 2 0 0 1-2 2H5'
        'a2 2 0 0 1-2-2V9Zm5 6h8m-3-3 3 3-3 3"/>',
    ),
    "read": (
        '<path d="m3 10 9 6 9-6v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2Z"/>',
        '<path d="m3 10 9-7 9 7v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-9Zm0 0 9 6 9-6"/>',
    ),
    "star": (
        '<path d="m12 3 2.8 5.7 6.2.9-4.5 4.4 1.1 6.2-5.6-3-5.6 3 1.1-6.2L3 9.6l6.2-.9Z"/>',
        '<path d="m12 3 2.8 5.7 6.2.9-4.5 4.4 1.1 6.2-5.6-3-5.6 3 1.1-6.2L3 9.6l6.2-.9Z"/>',
    ),
    "reply": (
        '<path d="m9 5-6 6 6 6v-4h4c4 0 6 2 8 5v-3c0-5-3-7-8-7H9Z"/>',
        '<path d="m9 5-6 6 6 6m-6-6h10c4 0 6 2 8 7"/>',
    ),
    "reply-all": (
        '<path d="m13 5-6 6 6 6v-4h2c3 0 5 2 7 5v-3c0-5-3-7-7-7h-2Z"/>',
        '<path d="m6.5 6-4.5 5 4.5 5m6.5-11-6 6 6 6m-6-6h8c3 0 4.5 2 6.5 7"/>',
    ),
    "forward": (
        '<path d="m15 5 6 6-6 6v-4h-4c-4 0-6 2-8 5v-3c0-5 3-7 8-7h4Z"/>',
        '<path d="m15 5 6 6-6 6m6-6H11c-4 0-6 2-8 7"/>',
    ),
    "sync": (
        '<circle cx="12" cy="12" r="6"/>',
        '<path d="M20 9a8.5 8.5 0 0 0-14-4L3 8m0-5v5h5'
        'm-4 7a8.5 8.5 0 0 0 14 4l3-3m0 5v-5h-5"/>',
    ),
    "edit": (
        '<path d="m4 15 12-12 5 5L9 20l-6 1Z"/>',
        '<path d="m4 15 12-12 5 5L9 20l-6 1 1-6Zm9-9 5 5"/>',
    ),
    "contact-add": (
        '<circle cx="9" cy="7" r="3"/><path d="M3 21v-3a6 6 0 0 1 12 0v3Z"/>',
        '<circle cx="9" cy="7" r="3"/><path d="M3 21v-3a6 6 0 0 1 12 0v3H3Zm16-15v8m-3-4h6"/>',
    ),
    "calendar-add": (
        '<path d="M3 10h18V7a2 2 0 0 0-2-2H5a2 2 0 0 0-2 2Z"/>',
        '<rect x="3" y="5" width="18" height="16" rx="2.5"/>'
        '<path d="M7 3v4m10-4v4M3 10h18m-9 3v5m-2.5-2.5h5"/>',
    ),
    "day": (
        '<rect x="8" y="12" width="8" height="6" rx="1"/>',
        '<rect x="3" y="5" width="18" height="16" rx="2.5"/>'
        '<path d="M7 3v4m10-4v4M3 10h18"/><rect x="8" y="12" width="8" height="6" rx="1"/>',
    ),
    "week": (
        '<path d="M9 10h6v11H9Z"/>',
        '<rect x="3" y="5" width="18" height="16" rx="2.5"/>'
        '<path d="M7 3v4m10-4v4M3 10h18M9 10v11m6-11v11"/>',
    ),
    "month": (
        '<path d="M3 10h18V7a2 2 0 0 0-2-2H5a2 2 0 0 0-2 2Z"/>',
        '<rect x="3" y="5" width="18" height="16" rx="2.5"/>'
        '<path d="M7 3v4m10-4v4M3 10h18m-12 0v11m6-11v11M3 15.5h18"/>',
    ),
    "agenda": (
        '<rect x="3" y="4" width="4" height="16" rx="1.5"/>',
        '<rect x="3" y="4" width="4" height="16" rx="1.5"/>'
        '<path d="M11 5h10M11 12h10M11 19h7"/>',
    ),
    "appearance": (
        '<path d="M12 3a9 9 0 1 0 0 18h1a2 2 0 0 0 1.4-3.4 1.5 1.5 0 0 1 1.1-2.6H17c6 0 5-12-5-12Z"/>',
        '<path d="M12 3a9 9 0 1 0 0 18h1a2 2 0 0 0 1.4-3.4 1.5 1.5 0 0 1 1.1-2.6H17c6 0 5-12-5-12Z"/>'
        '<path d="M7 11h.1M11 7h.1M16 8h.1M7 16h.1" stroke-width="2.5"/>',
    ),
    "layout": (
        '<path d="M3 8h6v13H5a2 2 0 0 1-2-2Z"/>',
        '<rect x="3" y="3" width="18" height="18" rx="2.5"/><path d="M3 8h18M9 8v13"/>',
    ),
    "settings": (
        '<path d="m10 3-1 3-3-1-2 3 2 2v4l-2 2 2 3 3-1 1 3h4l1-3 3 1 2-3-2-2v-4l2-2-2-3-3 1-1-3Z"/>',
        '<path d="m10 3-1 3-3-1-2 3 2 2v4l-2 2 2 3 3-1 1 3h4l1-3 3 1 2-3-2-2v-4l2-2-2-3-3 1-1-3Z"/>'
        '<circle cx="12" cy="12" r="3"/>',
    ),
    "history": (
        '<circle cx="12" cy="12" r="6"/>',
        '<path d="M3 10a9 9 0 1 1 1 7M3 4v6h6m3-4v6l4 2"/>',
    ),
    "imap": (
        '<rect x="3" y="4" width="12" height="6" rx="1.5"/>'
        '<rect x="3" y="14" width="12" height="6" rx="1.5"/>',
        '<rect x="3" y="4" width="12" height="6" rx="1.5"/>'
        '<rect x="3" y="14" width="12" height="6" rx="1.5"/>'
        '<path d="M6 7h.1M6 17h.1M20 4v16m-2-13 2-3 2 3m-4 10 2 3 2-3"/>',
    ),
    "pop3": (
        '<path d="M3 14h5l2 3h4l2-3h5v5a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2Z"/>',
        '<path d="M7 7H6l-3 7v5a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-5l-3-7h-1'
        'M3 14h5l2 3h4l2-3h5M12 3v9m-3-3 3 3 3-3"/>',
    ),
}


def main():
    destination = Path(__file__).resolve().parents[1] / "icons"
    for name, (fill, stroke) in ICONS.items():
        (destination / f"{name}.svg").write_text(
            '<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24">\n'
            f'  <g id="fill" fill="#DDE9E1">{fill}</g>\n'
            '  <g id="stroke" fill="none" stroke="#3F6654" stroke-width="1.8" '
            f'stroke-linecap="round" stroke-linejoin="round">{stroke}</g>\n'
            '</svg>\n'
        )


if __name__ == "__main__":
    main()

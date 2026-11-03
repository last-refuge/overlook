# Folder-only mailboxes

Some folders in an IMAP account can hold other folders but not mail. Overlook shows them
in the sidebar with a folder icon and dimmed text. Clicking one opens or closes the
folders inside it; nothing can be filed in it.

## Why they exist

An IMAP server can mark a folder as `\Noselect`. The name exists in the folder tree, but
the folder cannot be opened and cannot store messages. Servers create these in a few
common situations:

- **A folder was created inside a parent that does not exist.** Many mail clients create
  only the folder you asked for, say `Projects/2026/Taxes`, and let the server fill in
  `Projects` and `Projects/2026`. Dovecot, Courier and Cyrus all fill them in as
  folder-only entries.
- **The server's storage only allows one or the other.** Some storage layouts let a name be
  either a folder of messages or a directory of folders, never both. The directory side is
  folder-only.
- **The top of a shared or public namespace.** `Shared Folders`, `Public Folders` and
  `Other Users` are places to browse, not places to store mail.
- **Gmail's `[Gmail]` folder.** It only groups `All Mail`, `Sent Mail`, `Spam` and the other
  system folders. Overlook hides it because everything inside it is shown elsewhere.

## What you can do with one

- Expand or collapse it from the sidebar.
- Create a new folder inside it, from its right-click menu.
- Move a folder into it by dragging.

You cannot open it, file messages in it, rename it, delete it or add it to Favorites.
The server removes it on its own once the last folder inside it is gone. If you want
to keep mail at that place in the tree, create a folder inside it with the name you
want.

## Reference

The flag is defined in [RFC 3501, section 7.2.2](https://www.rfc-editor.org/rfc/rfc3501#section-7.2.2),
the IMAP specification.

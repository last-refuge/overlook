# Time zones

Introduced in 1.6.10. No new dependencies or schema migration (schema remains v19).

## Behavior

Settings → General → Time zone offers **Follow system** (default) and a searchable list of IANA zones, including UTC. An explicit selection takes effect without restart and applies to mail and calendar throughout the profile.

Follow system checks immediately after opening the window, then every 60 seconds. When the system zone differs from the last accepted zone, a banner displays both names and current UTC offsets:

- **Change time zone** accepts the detected zone and continues watching for future changes.
- **Keep current** persists the current zone as an override. Select Follow system in Settings to resume following.

Until answered, the banner leaves the effective zone unchanged; further checks update the candidate rather than opening duplicate prompts. If the system returns to the accepted zone, the banner disappears. A manually selected override suppresses prompts. The last accepted zone persists, so a system change while Overlook is closed is offered on the next launch too. The first launch starts in the detected system zone.

Mail list times and day groups, reader headers, reply/forward quote dates, print dates, draft timestamps, and last-sync clocks use the offset at each timestamp, including historical daylight-saving transitions. The old fixed startup offset and `OVERLOOK_UTC_OFFSET_MINUTES` development override are removed.

Detection reads `/etc/localtime` afresh on Linux, supporting both symlinks and replaced regular TZif files. It honors the standard `TZ` process environment variable when present (IANA name, POSIX rule, or absolute TZif path). A launch with `TZ` set follows that process zone. Startup detection failure falls back to UTC; a failed periodic read retains the accepted zone. Rendering uses cached rules and performs no system-zone file reads. Each check also repaints clocks and relative dates after midnight or system clock corrections, even under an override.

## Persistence and calendar dates

`Prefs.time_zone` is optional: absent/null follows the system; an override stores a `ZoneDefinition`. Most overrides are named IANA zones. Keeping a system zone without an IANA name stores its TZif rules, so it can be restored accurately after restart. The `calendar.time_zone` setting records the accepted projection separately from the detected system zone.

Zone changes use the ordered background settings writer. The chosen preference, projection marker, and reprojected Google/sample all-day events commit atomically before the active zone changes. Other preference saves preserve that timezone decision, including UI snapshots queued while the zone write is running. Failure retains the previous preference/zone and reports an error.

Stored message and timed-event instants stay in UTC. All-day start/end/recurrence dates retain their civil dates across zone changes and daylight-saving boundaries, including pending local edits. Calendar downloads made under an earlier projection cannot overwrite the new one or advance their sync tokens. Uploads use the saved event's zone for request/response mapping; an upload completing after a zone change reprojects its returned all-day dates before saving. Calendar coverage revision 4 forces one fresh Google download after upgrading.

Date-only invitation emails also retain their civil date in optional `Invitation.start_date` JSON metadata. Older stored invitations acquire that date using their previous projection before a zone change; fresh parses retain the literal ICS date, including mail downloaded while the zone changes.

CalDAV continues using zone-qualified projections, coverage, and pending-edit checks. Copied TZif files have a data fingerprint in their coverage key, since two different files can both be named “Local”. Existing cached data remains on disk; an old-zone projection is hidden until refreshed, and old-zone pending-edit guards remain in force.

An open event editor keeps the zone captured when it opened, displayed beside its timed fields. A zone change cannot silently reinterpret unsaved typed times. All-day fields remain calendar dates.

## Verification

Regression tests cover historical DST offsets, repeated fall-back hours, Yesterday across a 25-hour day, fractional-hour zones, same-offset named-zone changes, keeping/clearing overrides, copied `/etc/localtime` replacements, override serialization, all-day date preservation and restart, pending-edit protection, stale downloads/tokens, settings-save ordering, and uploads finishing across a zone change.

Run `cargo test --workspace --locked --no-fail-fast` and `cargo clippy --workspace --all-targets --locked`. These are headless checks; native banner/picker keyboard and pointer interaction, real desktop travel, and live account qualification still require a native-app check. The automated tests use disposable files and databases and do not change the real system timezone.

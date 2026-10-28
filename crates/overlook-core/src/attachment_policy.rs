//! Which files Overlook will attach to outgoing mail.
//!
//! A mail client has to send ordinary user files of any kind, so the policy is a blocklist
//! of executable and installer types, the same families Gmail refuses to send. A file is
//! refused when:
//!
//! - its extension is on [`BLOCKED_EXTENSIONS`];
//! - its content is an executable or script whatever it is called (ELF, PE/MZ, Mach-O,
//!   a `#!` script, a `.desktop` launcher); or
//! - it is a `.zip`, `.tar`, `.tar.gz` or `.tgz` archive containing any such entry by name,
//!   checked one level deep, as Gmail does.
//!
//! Accepted files are labelled with a MIME type derived from their extension, falling back
//! to `application/octet-stream`. Their names are cleaned ([`sanitize_filename`]) and a
//! message's attachments are capped at [`MAX_ATTACHMENTS_ENCODED_BYTES`] once encoded. The check runs when a file is attached and again when the
//! message is built for sending, so drafts restored from storage are covered too.

use std::io::Read;

/// Gmail's published list of blocked file types, plus Linux executables and packages.
pub const BLOCKED_EXTENSIONS: &[&str] = &[
    "ade", "adp", "apk", "appx", "appxbundle", "bat", "cab", "chm", "cmd", "com", "cpl", "diagcab", "diagcfg", "diagpkg", "dll", "dmg", "ex", "ex_", "exe", "hta", "img", "ins", "iso", "isp", "jar", "jnlp", "js", "jse", "lib", "lnk", "mde", "mjs", "msc", "msi", "msix", "msixbundle", "msp", "mst", "nsh", "pif", "ps1", "scr", "sct", "shb", "sys", "vb", "vbe", "vbs", "vhd", "vxd", "wsc", "wsf", "wsh", "xll",
    // Linux and macOS executables, launchers and packages.
    "sh", "bash", "zsh", "csh", "ksh", "run", "elf", "desktop", "appimage", "deb", "rpm", "flatpak", "flatpakref", "snap", "pkg", "command", "app",
];

/// Gmail and most providers refuse messages over 25 MB. Attachments travel base64-encoded
/// (4 bytes for every 3), so this allows about 18.75 MB of files per message.
pub const MAX_ATTACHMENTS_ENCODED_BYTES: u64 = 25 * 1024 * 1024;

/// The size a set of attachments occupies once base64-encoded for mail.
pub fn encoded_size(raw_bytes: u64) -> u64 {
    raw_bytes.div_ceil(3) * 4
}

/// Whether attachments totalling `raw_bytes` fit in one message.
pub fn within_size_limit(raw_bytes: u64) -> bool {
    encoded_size(raw_bytes) <= MAX_ATTACHMENTS_ENCODED_BYTES
}

/// The fixed wording when attachments would exceed the limit.
pub const TOO_LARGE: &str = "Attachments can total at most about 18 MB per message (25 MB once encoded for mail).";

/// Cleans a user-supplied file name before it is stored or sent: only the final path
/// component, no control or bidirectional-override characters (which can disguise
/// `photo<U+202E>gpj.exe` as `photoexe.jpg`), no leading dots, at most 180 characters.
pub fn sanitize_filename(name: &str) -> String {
    let last = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let cleaned: String = last
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{feff}'))
        .collect();
    let trimmed = cleaned.trim().trim_start_matches('.').trim();
    let mut out: String = trimmed.chars().take(180).collect();
    if out.is_empty() {
        out = "attachment".into();
    }
    out
}

/// Why a file was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The file's own extension is blocked.
    BlockedType { extension: String },
    /// The content is a program or script regardless of its name.
    ExecutableContent,
    /// An archive holds a blocked or executable entry.
    BlockedInArchive { entry: String },
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::BlockedType { extension } => write!(f, "Overlook doesn’t send .{extension} files, because they can run programs and most mail services block them."),
            Refusal::ExecutableContent => write!(f, "Overlook doesn’t send programs or scripts, because most mail services block them."),
            Refusal::BlockedInArchive { entry } => write!(f, "Overlook doesn’t send archives containing programs or scripts (“{entry}”), because most mail services block them."),
        }
    }
}

/// Checks a file about to be attached. `Ok` carries the MIME type to label it with.
pub fn check_outgoing(filename: &str, content: &[u8]) -> Result<&'static str, Refusal> {
    if let Some(extension) = blocked_extension(filename) {
        return Err(Refusal::BlockedType { extension });
    }
    if is_executable_content(content) {
        return Err(Refusal::ExecutableContent);
    }
    for entry in archive_entries(filename, content) {
        if blocked_extension(&entry).is_some() {
            return Err(Refusal::BlockedInArchive { entry });
        }
    }
    Ok(mime_type_for(filename))
}

/// Reads a file from disk and checks it. An unreadable file is reported as an I/O error.
pub fn check_outgoing_file(filename: &str, path: &std::path::Path) -> Result<Result<&'static str, Refusal>, std::io::Error> {
    let content = std::fs::read(path)?;
    Ok(check_outgoing(filename, &content))
}

/// The blocked extension of a name, lowercased, if any. Checks every dotted suffix so
/// `report.pdf.exe` and `setup.EXE` are caught, and ignores trailing dots and spaces that
/// some systems strip.
pub fn blocked_extension(filename: &str) -> Option<String> {
    let name = filename.rsplit(['/', '\\']).next().unwrap_or(filename).trim_end_matches(['.', ' ']).to_ascii_lowercase();
    let extension = name.rsplit_once('.')?.1;
    BLOCKED_EXTENSIONS.contains(&extension).then(|| extension.to_owned())
}

/// Recognizes programs and scripts by their first bytes.
pub fn is_executable_content(content: &[u8]) -> bool {
    const MAGICS: &[&[u8]] = &[
        b"\x7fELF",             // Linux/BSD executables and shared objects
        b"MZ",                  // Windows PE (exe, dll, scr, sys)
        b"\xfe\xed\xfa\xce",    // Mach-O 32-bit
        b"\xfe\xed\xfa\xcf",    // Mach-O 64-bit
        b"\xce\xfa\xed\xfe",    // Mach-O 32-bit, little-endian
        b"\xcf\xfa\xed\xfe",    // Mach-O 64-bit, little-endian
        b"\xca\xfe\xba\xbe",    // Mach-O universal binary (also Java class files)
        b"#!",                  // interpreter scripts
    ];
    if MAGICS.iter().any(|m| content.starts_with(m)) {
        return true;
    }
    let head = String::from_utf8_lossy(&content[..content.len().min(512)]);
    head.trim_start_matches('\u{feff}').trim_start().starts_with("[Desktop Entry]")
}

/// Entry names of a supported archive, one level deep. Anything unreadable yields no
/// names: the archive's own name and content checks still apply.
fn archive_entries(filename: &str, content: &[u8]) -> Vec<String> {
    let name = filename.to_ascii_lowercase();
    if content.starts_with(b"PK\x03\x04") || content.starts_with(b"PK\x05\x06") {
        return zip_entries(content);
    }
    if content.starts_with(&[0x1f, 0x8b]) && (name.ends_with(".tar.gz") || name.ends_with(".tgz")) {
        let mut tar = Vec::new();
        // Bound decompression so a gzip bomb cannot exhaust memory.
        let _ = flate2::read::GzDecoder::new(content).take(MAX_INFLATED_TAR).read_to_end(&mut tar);
        return tar_entries(&tar);
    }
    if name.ends_with(".tar") {
        return tar_entries(content);
    }
    Vec::new()
}

const MAX_INFLATED_TAR: u64 = 256 * 1024 * 1024;

/// File names from a zip's central directory. No entry is decompressed.
fn zip_entries(content: &[u8]) -> Vec<String> {
    let le16 = |b: &[u8], at: usize| b.get(at..at + 2).map(|s| u16::from_le_bytes([s[0], s[1]]) as usize);
    let le32 = |b: &[u8], at: usize| b.get(at..at + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]) as usize);
    // The end-of-central-directory record sits in the last 22 bytes plus any comment.
    let search_from = content.len().saturating_sub(22 + 65_535);
    let Some(end) = (search_from..content.len().saturating_sub(21)).rev().find(|&i| content[i..].starts_with(b"PK\x05\x06")) else { return Vec::new() };
    let (Some(count), Some(mut at)) = (le16(content, end + 10), le32(content, end + 16)) else { return Vec::new() };
    let mut names = Vec::new();
    for _ in 0..count {
        if !content.get(at..).is_some_and(|rest| rest.starts_with(b"PK\x01\x02")) {
            break;
        }
        let (Some(name_len), Some(extra_len), Some(comment_len)) = (le16(content, at + 28), le16(content, at + 30), le16(content, at + 32)) else { break };
        let Some(name) = content.get(at + 46..at + 46 + name_len) else { break };
        names.push(String::from_utf8_lossy(name).into_owned());
        at += 46 + name_len + extra_len + comment_len;
    }
    names
}

/// File names from a tar stream's headers (ustar prefix included).
fn tar_entries(content: &[u8]) -> Vec<String> {
    let field = |header: &[u8], range: std::ops::Range<usize>| {
        let raw = &header[range];
        String::from_utf8_lossy(&raw[..raw.iter().position(|&b| b == 0).unwrap_or(raw.len())]).into_owned()
    };
    let mut names = Vec::new();
    let mut at = 0;
    while let Some(header) = content.get(at..at + 512) {
        if header.iter().all(|&b| b == 0) {
            break;
        }
        let name = field(header, 0..100);
        let prefix = if &header[257..262] == b"ustar" { field(header, 345..500) } else { String::new() };
        names.push(if prefix.is_empty() { name } else { format!("{prefix}/{name}") });
        let size = usize::from_str_radix(field(header, 124..136).trim(), 8).unwrap_or(0);
        at += 512 + size.div_ceil(512) * 512;
    }
    names
}

/// The MIME type to declare for an accepted file, from its extension.
pub fn mime_type_for(filename: &str) -> &'static str {
    let extension = filename.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    match extension.as_str() {
        "pdf" => "application/pdf",
        "txt" | "log" => "text/plain",
        "csv" => "text/csv",
        "md" => "text/markdown",
        "html" | "htm" => "text/html",
        "ics" => "text/calendar",
        "vcf" => "text/vcard",
        "eml" => "message/rfc822",
        "json" => "application/json",
        "xml" => "application/xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "heic" => "image/heic",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "wav" => "audio/wav",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        "zip" => "application/zip",
        "gz" | "tgz" => "application/gzip",
        "tar" => "application/x-tar",
        "7z" => "application/x-7z-compressed",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "odt" => "application/vnd.oasis.opendocument.text",
        "ods" => "application/vnd.oasis.opendocument.spreadsheet",
        "odp" => "application/vnd.oasis.opendocument.presentation",
        "rtf" => "application/rtf",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zip_with(names: &[&str]) -> Vec<u8> {
        // Stored (uncompressed) empty entries: local headers, central directory, end record.
        let mut out = Vec::new();
        let mut central = Vec::new();
        for name in names {
            let offset = out.len() as u32;
            out.extend_from_slice(b"PK\x03\x04");
            out.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&[0, 0]);
            out.extend_from_slice(name.as_bytes());
            central.extend_from_slice(b"PK\x01\x02");
            central.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            central.extend_from_slice(&(name.len() as u16).to_le_bytes());
            central.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            central.extend_from_slice(&offset.to_le_bytes());
            central.extend_from_slice(name.as_bytes());
        }
        let central_at = out.len() as u32;
        out.extend_from_slice(&central);
        out.extend_from_slice(b"PK\x05\x06");
        out.extend_from_slice(&[0, 0, 0, 0]);
        out.extend_from_slice(&(names.len() as u16).to_le_bytes());
        out.extend_from_slice(&(names.len() as u16).to_le_bytes());
        out.extend_from_slice(&(central.len() as u32).to_le_bytes());
        out.extend_from_slice(&central_at.to_le_bytes());
        out.extend_from_slice(&[0, 0]);
        out
    }

    fn tar_with(names: &[&str]) -> Vec<u8> {
        let mut out = Vec::new();
        for name in names {
            let mut header = [0u8; 512];
            header[..name.len()].copy_from_slice(name.as_bytes());
            header[124..135].copy_from_slice(b"00000000005");
            header[257..262].copy_from_slice(b"ustar");
            out.extend_from_slice(&header);
            out.extend_from_slice(&[b'x'; 5]);
            out.extend_from_slice(&[0u8; 507]);
        }
        out.extend_from_slice(&[0u8; 1024]);
        out
    }

    #[test]
    fn ordinary_documents_pass_with_a_declared_type() {
        assert_eq!(check_outgoing("Q3 report.pdf", b"%PDF-1.7\n"), Ok("application/pdf"));
        assert_eq!(check_outgoing("photo.JPG", b"\xff\xd8\xff\xe0"), Ok("image/jpeg"));
        assert_eq!(check_outgoing("notes", b"plain text"), Ok("application/octet-stream"));
        assert_eq!(check_outgoing("bundle.zip", &zip_with(&["a.pdf", "b/c.docx"])), Ok("application/zip"));
        assert_eq!(check_outgoing("bundle.tar", &tar_with(&["a.pdf"])), Ok("application/x-tar"));
    }

    #[test]
    fn blocked_extensions_are_refused_whatever_the_case_or_decoration() {
        for name in ["setup.exe", "SETUP.EXE", "invoice.pdf.exe", "run.sh", "launcher.desktop", "tool.AppImage", "script.ps1", "trailing.exe.", "app.msi "] {
            assert!(matches!(check_outgoing(name, b"harmless"), Err(Refusal::BlockedType { .. })), "{name}");
        }
    }

    #[test]
    fn executable_content_is_refused_under_an_innocent_name() {
        for content in [&b"\x7fELF\x02\x01\x01"[..], b"MZ\x90\x00", b"\xcf\xfa\xed\xfe", b"#!/bin/sh\nrm -rf ~\n", b"\xef\xbb\xbf[Desktop Entry]\nExec=x\n"] {
            assert_eq!(check_outgoing("holiday-photos.jpg", content), Err(Refusal::ExecutableContent));
        }
    }

    #[test]
    fn archives_are_checked_one_level_deep() {
        assert_eq!(check_outgoing("photos.zip", &zip_with(&["a.jpg", "tools/setup.exe"])), Err(Refusal::BlockedInArchive { entry: "tools/setup.exe".into() }));
        assert_eq!(check_outgoing("renamed.dat", &zip_with(&["install.sh"])), Err(Refusal::BlockedInArchive { entry: "install.sh".into() }));
        assert_eq!(check_outgoing("src.tar", &tar_with(&["README", "build.sh"])), Err(Refusal::BlockedInArchive { entry: "build.sh".into() }));
        let mut gz = Vec::new();
        {
            use std::io::Write;
            let mut encoder = flate2::write::GzEncoder::new(&mut gz, flate2::Compression::fast());
            encoder.write_all(&tar_with(&["payload.exe"])).unwrap();
            encoder.finish().unwrap();
        }
        assert_eq!(check_outgoing("src.tar.gz", &gz), Err(Refusal::BlockedInArchive { entry: "payload.exe".into() }));
    }

    #[test]
    fn truncated_archives_do_not_panic() {
        let zip = zip_with(&["a.pdf"]);
        for cut in 0..zip.len() {
            let _ = check_outgoing("cut.zip", &zip[..cut]);
        }
        let tar = tar_with(&["a.pdf"]);
        let _ = check_outgoing("cut.tar", &tar[..700]);
    }

    #[test]
    fn filenames_are_cleaned() {
        assert_eq!(sanitize_filename("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_filename("C:\\Users\\x\\report.pdf"), "report.pdf");
        assert_eq!(sanitize_filename("photo\u{202e}gpj.exe"), "photogpj.exe");
        assert_eq!(sanitize_filename("tab\tname\n.txt"), "tabname.txt");
        assert_eq!(sanitize_filename("..hidden"), "hidden");
        assert_eq!(sanitize_filename("  "), "attachment");
        assert_eq!(sanitize_filename(&"a".repeat(400)).len(), 180);
        // A cleaned name still reveals a blocked extension that the override tried to hide.
        assert!(blocked_extension(&sanitize_filename("invoice\u{202e}fdp.exe")).is_some());
    }

    #[test]
    fn size_limit_counts_base64_overhead() {
        assert_eq!(encoded_size(3), 4);
        assert_eq!(encoded_size(4), 8);
        assert!(within_size_limit(18 * 1024 * 1024));
        assert!(!within_size_limit(19 * 1024 * 1024));
    }

    #[test]
    fn refusal_wording_is_fixed() {
        assert!(Refusal::ExecutableContent.to_string().starts_with("Overlook doesn’t send"));
        assert_eq!(Refusal::BlockedType { extension: "exe".into() }.to_string(), "Overlook doesn’t send .exe files, because they can run programs and most mail services block them.");
    }
}

//! A bounded IMAP search that uses the configured read-only session.

use super::*;
use std::time::Instant;

const UID_WINDOW: u32 = 20_000;

impl ImapProvider {
    pub(super) fn mcp_mail_search_in_folder(
        &self,
        folder: &str,
        query: &str,
        limit: usize,
        deadline: Instant,
    ) -> Result<(Vec<ProviderId>, bool), ProviderError> {
        let words: Vec<String> = query.split_whitespace().map(str::to_owned).collect();
        self.with_session_once(|conn| {
            if Instant::now() >= deadline {
                return Err(ProviderError::Offline);
            }
            let criteria = search_criteria(&words, conn.caps.literal_plus);
            if criteria.is_empty() || (!conn.caps.literal_plus && !query.is_ascii()) {
                return Err(ProviderError::Rejected(
                    "The IMAP server cannot search these terms".into(),
                ));
            }
            let mailbox = conn.examine(folder)?;
            if mailbox.exists == 0 {
                return Ok((Vec::new(), false));
            }
            let newest = mailbox
                .uid_next
                .and_then(|uid| uid.checked_sub(1))
                .filter(|&uid| uid > 0)
                .ok_or_else(|| {
                    ProviderError::Rejected("The IMAP server did not report UIDNEXT".into())
                })?;
            let validity = mailbox.uid_validity.filter(|&uid| uid > 0).ok_or_else(|| {
                ProviderError::Rejected("The IMAP server did not report UIDVALIDITY".into())
            })?;
            if Instant::now() >= deadline {
                return Err(ProviderError::Offline);
            }
            let oldest = newest.saturating_sub(UID_WINDOW - 1).max(1);
            // CHARSET is a command prefix and must remain the first argument.
            let hits = match conn
                .session()
                .uid_search(format!("{criteria} UID {oldest}:{newest}"))
            {
                Ok(hits) => hits,
                Err(error) => return Err(conn.err(&error)),
            };
            let mut hits: Vec<_> = hits
                .into_iter()
                .filter(|uid| (oldest..=newest).contains(uid))
                .collect();
            hits.sort_unstable_by(|a, b| b.cmp(a));
            let partial = oldest > 1 || hits.len() > limit;
            let ids = hits
                .into_iter()
                .take(limit)
                .map(|uid| MessageRef::provider_id(folder, validity, uid))
                .collect();
            Ok((ids, partial))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    #[test]
    fn mcp_search_examines_only_the_selected_folder_and_bounds_uids() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            write!(stream, "* OK ready\r\n").unwrap();
            loop {
                let mut line = String::new();
                assert_ne!(reader.read_line(&mut line).unwrap(), 0);
                let (tag, command) = line.trim_end().split_once(' ').unwrap();
                let response = match command {
                    "CAPABILITY" => "* CAPABILITY IMAP4rev1\r\n",
                    login if login.starts_with("LOGIN ") => "",
                    "EXAMINE \"Archive\"" => {
                        "* 3 EXISTS\r\n* OK [UIDNEXT 20400] next\r\n* OK [UIDVALIDITY 7] valid\r\n"
                    }
                    "UID SEARCH TEXT \"budget\" UID 400:20399" => {
                        write!(stream, "* SEARCH 300 401 20399\r\n{tag} OK done\r\n").unwrap();
                        break;
                    }
                    _ => panic!("unexpected server command {command}"),
                };
                write!(stream, "{response}{tag} OK done\r\n").unwrap();
            }
        });
        let endpoint = ServerEndpoint::new("127.0.0.1", port, Security::None, "sample");
        let provider = ImapProvider::new(
            MailServers {
                incoming: endpoint.clone(),
                outgoing: endpoint,
                pop3: None,
                imap: None,
                oauth: None,
            },
            "test",
        )
        .with_timeout(Duration::from_secs(2));
        let deadline = Instant::now() + Duration::from_secs(5);
        let (hits, partial) = provider
            .mcp_mail_search_in_folder("Archive", "budget", 1, deadline)
            .unwrap();
        assert_eq!(hits, [MessageRef::provider_id("Archive", 7, 20399)]);
        assert!(partial);
        server.join().unwrap();
    }
}

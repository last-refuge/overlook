//! Real Gmail HTTP client against an isolated loopback draft API.
use super::*;
use crate::gmail::{
    api::GmailClient,
    oauth::{ClientConfig, OAuthTokens},
    provider::GmailProvider,
};
use crate::MailProvider;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

#[derive(Default)]
struct State {
    items: BTreeMap<String, Value>,
    next: usize,
    creates: usize,
    updates: usize,
    lose_reply: bool,
    fail_list: bool,
    pages: usize,
    sends: usize,
    last_sent: Option<Value>,
}

struct Server {
    endpoint: String,
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Server {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(State::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (worker_state, worker_stop) = (state.clone(), stop.clone());
        let worker = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if worker_stop.load(Ordering::SeqCst) {
                    break;
                }
                let mut stream = stream.unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let words: Vec<_> = line.split_whitespace().collect();
                let (method, target) = (words[0].to_owned(), words[1].to_owned());
                let mut length = 0;
                let mut chunked = false;
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).unwrap();
                    if header == "\r\n" {
                        break;
                    }
                    if let Some((key, value)) = header.split_once(':') {
                        if key.eq_ignore_ascii_case("content-length") {
                            length = value.trim().parse().unwrap();
                        }
                        if key.eq_ignore_ascii_case("transfer-encoding") {
                            chunked = value.trim().eq_ignore_ascii_case("chunked");
                        }
                    }
                }
                let mut bytes = vec![0; length];
                reader.read_exact(&mut bytes).unwrap();
                if chunked {
                    loop {
                        let mut size = String::new();
                        reader.read_line(&mut size).unwrap();
                        let n = usize::from_str_radix(size.trim(), 16).unwrap();
                        if n == 0 {
                            break;
                        }
                        let start = bytes.len();
                        bytes.resize(start + n, 0);
                        reader.read_exact(&mut bytes[start..]).unwrap();
                        let mut crlf = [0; 2];
                        reader.read_exact(&mut crlf).unwrap();
                    }
                }
                let input: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
                let url = url::Url::parse(&format!("http://localhost{target}")).unwrap();
                let mut state = worker_state.lock().unwrap();
                let path = url.path().trim_start_matches('/');
                let id = path.strip_prefix("drafts/");
                let (status, value) = match (method.as_str(), path, id) {
                    ("POST", "drafts/send", _) => {
                        let id = input["id"].as_str().unwrap();
                        if let Some(old) = state.items.remove(id) {
                            state.sends += 1;
                            state.last_sent = Some(input.clone());
                            (
                                200,
                                json!({"id": format!("sent-{}",state.sends),"threadId":old["message"]["threadId"]}),
                            )
                        } else {
                            (404, json!({}))
                        }
                    }
                    ("GET", "drafts", _) => {
                        state.pages += 1;
                        if state.fail_list {
                            (503, json!({"error":{"message":"temporary"}}))
                        } else {
                            let offset = url
                                .query_pairs()
                                .find(|(k, _)| k == "pageToken")
                                .map(|(_, v)| v.parse::<usize>().unwrap())
                                .unwrap_or(0);
                            let items: Vec<Value> = state.items.values().skip(offset).take(1).map(|v| json!({"id":v["id"],"message":{"id":v["message"]["id"],"threadId":v["message"]["threadId"]}})).collect();
                            let mut value = json!({"drafts":items});
                            if offset + 1 < state.items.len() {
                                value["nextPageToken"] = json!((offset + 1).to_string());
                            }
                            (200, value)
                        }
                    }
                    ("GET", _, Some(id)) => match state.items.get(id) {
                        Some(item) => {
                            let mut item = item.clone();
                            if url
                                .query_pairs()
                                .any(|(k, v)| k == "format" && v == "metadata")
                            {
                                let raw = super::super::mapping::base64url_decode(
                                    item["message"]["raw"].as_str().unwrap(),
                                );
                                let parsed =
                                    mail_parser::MessageParser::default().parse(&raw).unwrap();
                                item["message"]["payload"] = json!({"headers":[{"name":"Message-ID","value":format!("<{}>", parsed.message_id().unwrap())}]});
                                item["message"].as_object_mut().unwrap().remove("raw");
                            }
                            (200, item)
                        }
                        None => (404, json!({})),
                    },
                    ("POST", "drafts", _) | ("PUT", _, Some(_)) => {
                        if method == "PUT" && !state.items.contains_key(id.unwrap()) {
                            (404, json!({}))
                        } else {
                            state.next += 1;
                            let id = id
                                .map(str::to_owned)
                                .unwrap_or_else(|| format!("d{}", state.next));
                            let item = json!({"id":id,"message":{"id":format!("m{}",state.next),"threadId": input["message"].get("threadId").cloned().unwrap_or(json!("thread-1")),"raw":input["message"]["raw"]}});
                            state.items.insert(id, item.clone());
                            if method == "POST" {
                                state.creates += 1;
                            } else {
                                state.updates += 1;
                            }
                            if std::mem::take(&mut state.lose_reply) {
                                continue;
                            }
                            (200, item)
                        }
                    }
                    ("DELETE", _, Some(id)) => {
                        state.items.remove(id);
                        (204, Value::Null)
                    }
                    _ => (404, json!({})),
                };
                drop(state);
                let body = if status == 204 {
                    String::new()
                } else {
                    value.to_string()
                };
                write!(stream, "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
        });
        Self {
            endpoint,
            state,
            stop,
            worker: Some(worker),
        }
    }

    fn provider(&self) -> GmailProvider {
        let now = overlook_core::time::now();
        let client = GmailClient::new(
            ClientConfig {
                client_id: "synthetic".into(),
                client_secret: None,
            },
            OAuthTokens {
                access_token: "synthetic".into(),
                refresh_token: None,
                expires_at: now + 3_600_000,
                scope: None,
                signed_in_at: now,
            },
            Box::new(|_| {}),
        )
        .with_test_endpoint(self.endpoint.clone());
        GmailProvider::new(Arc::new(client))
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.endpoint.trim_start_matches("http://"));
        self.worker.take().unwrap().join().unwrap();
    }
}

fn block<F: std::future::Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    match future.as_mut().poll(&mut context) {
        std::task::Poll::Ready(result) => result,
        _ => panic!("blocking HTTP adapter yielded"),
    }
}

#[test]
fn gmail_draft_api_paginates_updates_and_recovers_a_lost_create_response() {
    let server = Server::start();
    let provider = server.provider();
    let account = Account {
        id: AccountId::from("gmail-test"),
        provider: ProviderKind::Gmail,
        display_name: "Test".into(),
        email: "me@example.com".into(),
        created_at: 0,
    };
    let raw = |token: &str, body: &str| {
        format!("From: me@example.com\r\nMessage-ID: <{token}@example.com>\r\nSubject: Test\r\n\r\n{body}").into_bytes()
    };
    let first = block(provider.put_draft(
        &account,
        None,
        &raw("first", "First"),
        "<first@example.com>",
        Some("original-thread"),
    ))
    .unwrap()
    .unwrap();
    let second = block(provider.put_draft(
        &account,
        None,
        &raw("second", "Second"),
        "<second@example.com>",
        None,
    ))
    .unwrap()
    .unwrap();
    let list = block(provider.list_drafts(&account)).unwrap();
    assert_eq!(list.len(), 2);
    assert!(list.iter().any(|r| r.id == second.id));
    assert!(server.state.lock().unwrap().pages >= 2);
    assert_eq!(first.thread_id.as_deref(), Some("original-thread"));
    let updated = block(provider.put_draft(
        &account,
        Some(&first),
        &raw("edit", "Edited"),
        "<edit@example.com>",
        first.thread_id.as_deref(),
    ))
    .unwrap()
    .unwrap();
    assert_eq!(updated.id, first.id);
    assert_ne!(updated.revision, first.revision);
    assert!(block(provider.fetch_draft(&account, &first))
        .unwrap()
        .is_none());
    assert!(String::from_utf8(
        block(provider.fetch_draft(&account, &updated))
            .unwrap()
            .unwrap()
    )
    .unwrap()
    .contains("Edited"));
    assert_eq!(
        block(provider.put_draft(
            &account,
            Some(&first),
            &raw("edit", "Edited"),
            "<edit@example.com>",
            None
        ))
        .unwrap(),
        Some(updated.clone())
    );
    assert!(block(provider.put_draft(
        &account,
        Some(&first),
        &raw("stale", "Stale"),
        "<stale@example.com>",
        None
    ))
    .unwrap()
    .is_none());
    assert_eq!(server.state.lock().unwrap().updates, 1);
    server.state.lock().unwrap().lose_reply = true;
    assert!(block(provider.put_draft(
        &account,
        None,
        &raw("lost", "Lost response"),
        "<lost@example.com>",
        None
    ))
    .is_err());
    let recovered = block(provider.put_draft(
        &account,
        None,
        &raw("lost", "Lost response"),
        "<lost@example.com>",
        None,
    ))
    .unwrap()
    .unwrap();
    assert_eq!(server.state.lock().unwrap().creates, 3);
    {
        let mut state = server.state.lock().unwrap();
        let item = state.items.get_mut(&recovered.id.0).unwrap();
        item["message"]["raw"] = json!(base64url_encode(&raw(
            "lost",
            "Edited elsewhere with the same Message-ID"
        )));
        item["message"]["id"] = json!("changed-elsewhere");
    }
    assert!(
        block(provider.put_draft(
            &account,
            None,
            &raw("lost", "Lost response"),
            "<lost@example.com>",
            None
        ))
        .unwrap()
        .is_none(),
        "a retained Message-ID must not hide another client's edits"
    );
    assert!(
        block(provider.delete_remote_draft(&account, &recovered)).is_err(),
        "deleting an old version must not erase another client's edits"
    );
    for remote in block(provider.list_drafts(&account)).unwrap() {
        block(provider.delete_remote_draft(&account, &remote)).unwrap();
    }
    block(provider.delete_remote_draft(&account, &first)).unwrap();
    assert!(block(provider.list_drafts(&account)).unwrap().is_empty());
    server.state.lock().unwrap().fail_list = true;
    assert!(
        block(provider.list_drafts(&account)).is_err(),
        "failed listing must never resemble an empty mailbox"
    );
}

#[test]
fn gmail_sends_the_existing_draft_and_does_not_resend_a_missing_draft() {
    let server = Server::start();
    let provider = server.provider();
    let account = Account {
        id: AccountId::from("gmail-send-test"),
        provider: ProviderKind::Gmail,
        display_name: "Test".into(),
        email: "me@example.com".into(),
        created_at: 0,
    };
    let raw = b"From: me@example.com\r\nTo: friend@example.com\r\nBcc: hidden@example.com\r\nMessage-ID: <draft-send@example.com>\r\nSubject: Draft\r\n\r\nBody";
    let remote = block(provider.put_draft(
        &account,
        None,
        raw,
        "<draft-send@example.com>",
        Some("reply-thread"),
    ))
    .unwrap()
    .unwrap();
    let draft = crate::drafts::import(&account, &remote, raw).unwrap().draft;
    block(provider.send(&account, &draft, crate::SendContext::default())).unwrap();
    let state = server.state.lock().unwrap();
    assert!(state.items.is_empty());
    assert_eq!(state.sends, 1);
    let sent = state.last_sent.as_ref().unwrap();
    assert_eq!(sent["id"].as_str(), Some(remote.id.as_str()));
    assert_eq!(sent["message"]["threadId"], "reply-thread");
    let bytes = super::super::mapping::base64url_decode(sent["message"]["raw"].as_str().unwrap());
    assert!(String::from_utf8(bytes)
        .unwrap()
        .contains("Bcc: hidden@example.com"));
    drop(state);
    assert!(block(provider.send(&account, &draft, crate::SendContext::default())).is_err());
    assert_eq!(server.state.lock().unwrap().sends, 1);
}

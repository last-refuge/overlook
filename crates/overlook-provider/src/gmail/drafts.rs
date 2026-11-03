use super::api::{ApiError, GmailClient};
use super::mapping::base64url_encode;
use super::oauth::url_encode;
use crate::ProviderError;
use base64::Engine;
use overlook_core::*;
use serde_json::{json, Value};
use std::collections::HashSet;

#[cfg(test)]
#[path = "draft_tests.rs"]
mod tests;

fn descriptor(value: &Value) -> Result<RemoteDraft, ProviderError> {
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let message = value.get("message");
    let revision = message
        .and_then(|m| m.get("id"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    match (id, revision) {
        (Some(id), Some(revision)) => Ok(RemoteDraft {
            id: ProviderId(id.into()),
            revision: revision.into(),
            thread_id: message
                .and_then(|m| m.get("threadId"))
                .and_then(Value::as_str)
                .map(str::to_owned),
        }),
        _ => Err(ProviderError::Temporary(
            "Gmail returned an incomplete draft listing".into(),
        )),
    }
}

pub(super) fn list(client: &GmailClient) -> Result<Vec<RemoteDraft>, ProviderError> {
    let mut drafts = Vec::new();
    let mut page = None;
    let mut pages = HashSet::new();
    loop {
        let mut query = vec![("maxResults", "500")];
        if let Some(page) = page.as_deref() {
            query.push(("pageToken", page));
        }
        let v = client.get("drafts", &query)?;
        if !v.is_object() {
            return Err(ProviderError::Temporary(
                "Gmail returned an invalid draft listing".into(),
            ));
        }
        if let Some(items) = v.get("drafts") {
            let items = items.as_array().ok_or_else(|| {
                ProviderError::Temporary("Gmail returned an invalid draft listing".into())
            })?;
            for item in items {
                drafts.push(descriptor(item)?);
            }
        }
        page = match v.get("nextPageToken") {
            None => None,
            Some(token) => Some(
                token
                    .as_str()
                    .filter(|t| !t.is_empty())
                    .ok_or_else(|| {
                        ProviderError::Temporary(
                            "Gmail returned an invalid draft page token".into(),
                        )
                    })?
                    .to_owned(),
            ),
        };
        match &page {
            None => break,
            Some(p) if !pages.insert(p.clone()) => {
                return Err(ProviderError::Temporary(
                    "Gmail repeated a draft page".into(),
                ))
            }
            _ => {}
        }
    }
    Ok(drafts)
}

pub(super) fn fetch(
    client: &GmailClient,
    remote: &RemoteDraft,
) -> Result<Option<Vec<u8>>, ProviderError> {
    let v = match client.get(
        &format!("drafts/{}", url_encode(&remote.id.0)),
        &[("format", "raw")],
    ) {
        Ok(v) => v,
        Err(ApiError::NotFound) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if descriptor(&v)?.revision != remote.revision {
        return Ok(None);
    }
    let raw = v
        .pointer("/message/raw")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            ProviderError::Temporary("Gmail returned a draft without its content".into())
        })?;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(raw.trim_end_matches('='))
        .map(Some)
        .map_err(|_| ProviderError::Temporary("Gmail returned invalid draft content".into()))
}

fn metadata(client: &GmailClient, id: &str) -> Result<Option<Value>, ProviderError> {
    match client.get(
        &format!("drafts/{}", url_encode(id)),
        &[("format", "metadata")],
    ) {
        Ok(v) => Ok(Some(v)),
        Err(ApiError::NotFound) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn has_message_id(v: &Value, id: &str) -> bool {
    v.pointer("/message/payload/headers")
        .and_then(Value::as_array)
        .is_some_and(|headers| {
            headers.iter().any(|h| {
                h.get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|n| n.eq_ignore_ascii_case("Message-ID"))
                    && h.get("value").and_then(Value::as_str).is_some_and(|v| {
                        v.trim().trim_matches(['<', '>']) == id.trim_matches(['<', '>'])
                    })
            })
        })
}

pub(super) fn put(
    client: &GmailClient,
    remote: Option<&RemoteDraft>,
    raw: &[u8],
    message_id: &str,
    thread_id: Option<&str>,
) -> Result<Option<RemoteDraft>, ProviderError> {
    if let Some(remote) = remote {
        let Some(v) = metadata(client, &remote.id.0)? else {
            return Ok(None);
        };
        let current = descriptor(&v)?;
        if has_message_id(&v, message_id) {
            return match fetch(client, &current)? {
                Some(saved) if crate::drafts::same_content(&saved, raw)? => Ok(Some(current)),
                _ => Ok(None),
            };
        }
        if current.revision != remote.revision {
            return Ok(None);
        }
    } else {
        // A create's response can be lost. List directly rather than rely on search's
        // indexing delay; only this upload's persisted Message-ID can be adopted.
        for item in list(client)? {
            if let Some(v) = metadata(client, &item.id.0)? {
                if has_message_id(&v, message_id) {
                    let current = descriptor(&v)?;
                    return match fetch(client, &current)? {
                        Some(saved) if crate::drafts::same_content(&saved, raw)? => {
                            Ok(Some(current))
                        }
                        _ => Ok(None),
                    };
                }
            }
        }
    }
    let mut message = json!({"raw": base64url_encode(raw)});
    if let Some(thread) = thread_id {
        message["threadId"] = json!(thread);
    }
    let body = json!({"message": message});
    let result = match remote {
        Some(remote) => client.put(&format!("drafts/{}", url_encode(&remote.id.0)), &body),
        None => client.post("drafts", &body),
    };
    match result {
        Ok(v) => descriptor(&v).map(Some),
        Err(ApiError::NotFound) if remote.is_some() => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub(super) fn delete(client: &GmailClient, remote: &RemoteDraft) -> Result<(), ProviderError> {
    let Some(value) = metadata(client, &remote.id.0)? else {
        return Ok(());
    };
    if descriptor(&value)?.revision != remote.revision {
        return Err(ProviderError::Temporary(
            "A discarded draft changed on another device; it will be preserved on the next sync"
                .into(),
        ));
    }
    match client.delete(&format!("drafts/{}", url_encode(&remote.id.0))) {
        Ok(_) | Err(ApiError::NotFound) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

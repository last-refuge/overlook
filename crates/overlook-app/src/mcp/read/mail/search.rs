use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use overlook_core::{Label, LabelKind, ProviderId, ProviderKind};
use overlook_store::McpMailSelection;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use super::{not_cached, parse, storage, summary, ErrorCode, Failure, ReadContext, FOLDER, SEARCH};

const MAX_PROVIDER_FOLDERS: usize = 12;
const MAX_PROVIDER_HITS: usize = 200;
const PROVIDER_WAIT: Duration = Duration::from_secs(3);
static PROVIDER_BUSY: AtomicBool = AtomicBool::new(false);

struct ProviderSlot;

impl Drop for ProviderSlot {
    fn drop(&mut self) {
        PROVIDER_BUSY.store(false, Ordering::Release);
    }
}

pub(super) fn schema(search: bool) -> Value {
    let mut properties = json!({
        "folder_id": {"type": "string", "description": "Opaque ID from mail_folders_list."},
        "after": {"type": "integer", "description": "Inclusive message date, Unix milliseconds UTC."},
        "before": {"type": "integer", "description": "Exclusive message date, Unix milliseconds UTC."}
    });
    if search {
        properties["query"] = json!({"type":"string","minLength":1,"maxLength":256});
        properties["source"] =
            json!({"type":"string","enum":["cache","provider"],"default":"cache"});
    }
    super::schema(properties, if search { &["query"] } else { &[] }, true)
}

#[derive(Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Source {
    #[default]
    Cache,
    Provider,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    query: Option<String>,
    folder_id: Option<String>,
    after: Option<i64>,
    before: Option<i64>,
    #[serde(default)]
    source: Source,
    #[serde(rename = "limit")]
    _limit: Option<usize>,
    #[serde(rename = "cursor")]
    _cursor: Option<String>,
}

impl Input {
    fn validate(&self, search: bool) -> Result<(), Failure> {
        if search
            && !self.query.as_deref().is_some_and(|query| {
                !query.trim().is_empty()
                    && query.len() <= 256
                    && !query.chars().any(char::is_control)
                    && query.chars().any(char::is_alphanumeric)
            })
        {
            return Err(Failure::new(
                ErrorCode::InvalidRequest,
                "Query must contain letters or digits and fit in 256 bytes",
            ));
        }
        if self
            .after
            .zip(self.before)
            .is_some_and(|(after, before)| after >= before)
        {
            return Err(Failure::new(
                ErrorCode::InvalidRequest,
                "after must be earlier than before",
            ));
        }
        Ok(())
    }
}

pub(super) fn execute(
    ctx: &ReadContext<'_>,
    name: &str,
    args: &Map<String, Value>,
) -> Result<Value, Failure> {
    let input: Input = parse(args)?;
    input.validate(name == SEARCH)?;
    let mut labels = ctx.store.labels(&ctx.account.id).map_err(storage)?;
    labels.sort_unstable_by_key(|label| {
        (
            label.kind != LabelKind::Inbox,
            label.sort_order,
            label.id.clone(),
        )
    });
    let folder = input
        .folder_id
        .as_ref()
        .map(|id| {
            labels
                .iter()
                .find(|label| ctx.id(FOLDER, label.id.as_str()) == *id)
                .ok_or_else(|| not_cached("Folder is unavailable in the selected account's cache"))
        })
        .transpose()?;
    if folder.is_some_and(|label| !cache_searchable(label)) {
        return Err(Failure::new(
            ErrorCode::Unsupported,
            "This folder does not contain searchable cached messages",
        ));
    }

    let selection = match (input.source == Source::Provider, input.query.as_deref()) {
        (true, _) => McpMailSelection::Coverage,
        (false, Some(query)) => McpMailSelection::Search(query),
        (false, None) => McpMailSelection::Headers,
    };
    let cache = ctx
        .store
        .mcp_mail_cache_search(
            &ctx.account.id,
            folder.map(|label| &label.id),
            input.after,
            input.before,
            selection,
        )
        .map_err(storage)?;
    let mut coverage = json!({
        "account_id": ctx.account_id,
        "folder_id": input.folder_id,
        "after": input.after,
        "before": input.before,
        "cache_scope": if folder.is_some() { "selected_folder" } else { "selected_account" },
        "cached_message_count": cache.coverage.messages_indexed,
        "cached_body_count": cache.coverage.bodies_cached,
        "cached_headers_without_body_count": cache.coverage.messages_indexed.saturating_sub(cache.coverage.bodies_cached),
        "cached_oldest_date": cache.oldest,
        "cached_newest_date": cache.newest,
    });
    let (messages, source) = if input.source == Source::Provider {
        let (matches, evidence) = provider_search(ctx, &input, &labels, folder)?;
        coverage["partial"] = evidence["partial"].clone();
        coverage["provider"] = evidence;
        (matches, "provider")
    } else {
        coverage["result_limit_reached"] = json!(cache.result_limit_reached);
        coverage["partial"] = json!(
            ctx.account.provider != ProviderKind::Sample
                || cache.result_limit_reached
                || cache.coverage.messages_indexed > cache.coverage.bodies_cached
        );
        coverage["provider_mail_searched"] = json!(false);
        (cache.messages, "cache")
    };
    let mut data = ctx.page(
        messages.iter().map(|m| summary(ctx, m)).collect(),
        name,
        args,
    )?;
    data["source"] = json!(source);
    data["coverage"] = coverage;
    Ok(data)
}

fn cache_searchable(label: &Label) -> bool {
    !label.kind.is_virtual() && label.kind != LabelKind::Container
}

fn provider_searchable(label: &Label) -> bool {
    cache_searchable(label)
        && label.provider_id.is_some()
        && !matches!(label.kind, LabelKind::Spam | LabelKind::Trash)
}

enum RemoteScope {
    Sample,
    Imap(Vec<(String, String)>),
}

struct RemoteHits {
    ids: Vec<ProviderId>,
    searched_folders: Vec<String>,
    refused_folders: Vec<String>,
    partial: bool,
    unsearched_folders: usize,
}

fn provider_search(
    ctx: &ReadContext<'_>,
    input: &Input,
    labels: &[Label],
    folder: Option<&Label>,
) -> Result<(Vec<overlook_core::Message>, Value), Failure> {
    let (scope, excluded, omitted) = match ctx.account.provider {
        ProviderKind::Sample => (RemoteScope::Sample, 0, 0),
        ProviderKind::Imap => {
            if folder.is_some_and(|label| !provider_searchable(label)) {
                return Err(Failure::new(
                    ErrorCode::Unsupported,
                    "Provider search is unavailable for this folder",
                ));
            }
            let candidates = folder.map_or(labels, std::slice::from_ref);
            let eligible: Vec<_> = candidates
                .iter()
                .filter(|l| provider_searchable(l))
                .collect();
            let paths = eligible
                .iter()
                .take(MAX_PROVIDER_FOLDERS)
                .filter_map(|l| {
                    Some((
                        ctx.id(FOLDER, l.id.as_str()),
                        l.provider_id.as_ref()?.0.clone(),
                    ))
                })
                .collect();
            let excluded = candidates.len() - eligible.len();
            let omitted = eligible.len().saturating_sub(MAX_PROVIDER_FOLDERS);
            (RemoteScope::Imap(paths), excluded, omitted)
        }
        _ => {
            return Err(Failure::new(
                ErrorCode::Unsupported,
                "This account does not support provider mail search",
            ))
        }
    };
    let sample = matches!(scope, RemoteScope::Sample);
    let provider = ctx.mail_provider()?;
    if PROVIDER_BUSY.swap(true, Ordering::AcqRel) {
        return Err(Failure::new(
            ErrorCode::ProviderUnavailable,
            "A provider search is already in progress",
        ));
    }
    let slot = ProviderSlot;
    let account = ctx.account.clone();
    let query = input.query.as_deref().unwrap_or_default().trim().to_owned();
    let deadline = Instant::now() + PROVIDER_WAIT;
    let runtime = ctx.runtime.handle().clone();
    let (sender, receiver) = mpsc::sync_channel(1);
    ctx.runtime.spawn_blocking(move || {
        let _slot = slot;
        let result = runtime.block_on(async {
            let mut hits = RemoteHits {
                ids: Vec::new(),
                searched_folders: Vec::new(),
                refused_folders: Vec::new(),
                partial: false,
                unsearched_folders: 0,
            };
            if Instant::now() >= deadline {
                return Err(overlook_provider::ProviderError::Offline);
            }
            match scope {
                RemoteScope::Sample => {
                    hits.ids = provider
                        .search(&account, &query, MAX_PROVIDER_HITS + 1)
                        .await?;
                    hits.partial = hits.ids.len() > MAX_PROVIDER_HITS;
                    hits.ids.truncate(MAX_PROVIDER_HITS);
                }
                RemoteScope::Imap(paths) => {
                    let count = paths.len();
                    for (index, (id, path)) in paths.into_iter().enumerate() {
                        if Instant::now() >= deadline || hits.ids.len() == MAX_PROVIDER_HITS {
                            hits.unsearched_folders = count - index;
                            break;
                        }
                        match provider
                            .mcp_mail_search_folder(
                                &path,
                                &query,
                                MAX_PROVIDER_HITS - hits.ids.len(),
                                deadline,
                            )
                            .await
                        {
                            Ok((matches, partial)) => {
                                hits.searched_folders.push(id);
                                hits.ids.extend(matches);
                                hits.partial |= partial;
                            }
                            Err(overlook_provider::ProviderError::Rejected(_)) => {
                                hits.refused_folders.push(id)
                            }
                            Err(error) => return Err(error),
                        }
                    }
                }
            }
            Ok(hits)
        });
        let _ = sender.send(result);
    });
    let unavailable = || {
        Failure::new(
            ErrorCode::ProviderUnavailable,
            "Provider search could not finish. Cached search is available.",
        )
    };
    let remote = receiver
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .map_err(|_| unavailable())?
        .map_err(|_| unavailable())?;
    if remote.searched_folders.is_empty() && !remote.refused_folders.is_empty() {
        return Err(Failure::new(
            ErrorCode::ProviderUnavailable,
            "The provider refused to search the requested folders",
        ));
    }
    let total = remote.ids.len();
    let mut matches = ctx
        .store
        .mcp_mail_provider_headers(&ctx.account.id, &remote.ids)
        .map_err(storage)?;
    let cached_count = matches
        .iter()
        .filter_map(|m| m.provider_id.as_ref())
        .collect::<HashSet<_>>()
        .len();
    matches.retain(|m| {
        (!sample || folder.is_none_or(|label| m.has_label(&label.id)))
            && input.after.is_none_or(|after| m.date >= after)
            && input.before.is_none_or(|before| m.date < before)
    });
    matches.sort_unstable_by(|a, b| b.date.cmp(&a.date).then(a.id.cmp(&b.id)));
    let evidence = json!({
        "scope": if sample { "sample_mailbox" } else { "known_imap_folders" },
        "searched_folder_ids": remote.searched_folders,
        "refused_folder_ids": remote.refused_folders,
        "excluded_folder_count": excluded,
        "folders_omitted_by_limit": omitted + remote.unsearched_folders,
        "uid_window_or_result_limit_reached": remote.partial,
        "provider_match_count": total,
        "matches_missing_from_cache": total.saturating_sub(cached_count),
        "matched_cached_message_count_after_filters": matches.len(),
        "filters_use_cached_metadata": (sample && input.folder_id.is_some()) || input.after.is_some() || input.before.is_some(),
        "partial": !sample || remote.partial || cached_count < total,
    });
    Ok((matches, evidence))
}

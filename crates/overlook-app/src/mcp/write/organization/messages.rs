use super::*;
use overlook_core::{LabelId, Message, MessageId};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Action {
    Read,
    Unread,
    Star,
    Unstar,
    Archive,
    Trash,
    Restore,
    Move,
    AddLabel,
    RemoveLabel,
    Junk,
    NotJunk,
}

impl Action {
    fn takes_folder(self) -> bool {
        matches!(self, Self::Move | Self::AddLabel | Self::RemoveLabel)
    }

    fn title(self) -> &'static str {
        match self {
            Self::Read => "Marked as read",
            Self::Unread => "Marked as unread",
            Self::Star => "Starred",
            Self::Unstar => "Unstarred",
            Self::Archive => "Archived",
            Self::Trash => "Moved to Trash",
            Self::Restore => "Moved to Inbox",
            Self::Move => "Moved messages",
            Self::AddLabel => "Added label",
            Self::RemoveLabel => "Removed label",
            Self::Junk => "Marked as junk",
            Self::NotJunk => "Marked as not junk",
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Organize {
    message_ids: Vec<String>,
    action: Action,
    folder_id: Option<String>,
}

fn select(
    ctx: &WriteContext<'_>,
    tx: &McpWriteTxn<'_, '_>,
    ids: &[String],
) -> Result<BTreeMap<String, Message>, Failure> {
    if !(1..=MAX_MESSAGES).contains(&ids.len()) {
        return Err(invalid("Specify between 1 and 50 message IDs"));
    }
    let mut requested = BTreeSet::new();
    for id in ids {
        validate_id(id)?;
        if !requested.insert(id.as_str()) {
            return Err(invalid("Message IDs must be unique"));
        }
    }
    tx.organization_messages(&requested, |id| ctx.id(MESSAGE_ID, id))
        .map_err(storage)
}

pub(super) fn organize(
    ctx: &WriteContext<'_>,
    tx: &McpWriteTxn<'_, '_>,
    input: Organize,
) -> Result<Value, Failure> {
    let Organize {
        message_ids,
        action,
        folder_id,
    } = input;
    if action.takes_folder() != folder_id.is_some() {
        return Err(invalid(
            "Specify a folder ID only for move, add_label, or remove_label",
        ));
    }
    let messages = select(ctx, tx, &message_ids)?;
    let labels = tx.organization_labels().map_err(storage)?;
    let imap = ctx.account.provider == ProviderKind::Imap;
    let folder = folder_id
        .as_deref()
        .map(|id| lookup_folder(ctx, &labels, id))
        .transpose()?;
    let target = target_for(action, folder, &labels, imap)?;
    let mailbox = Mailbox {
        labels: &labels,
        target,
        imap,
    };
    let mut items = Vec::with_capacity(message_ids.len());
    let mut changed = Vec::new();
    let mut forward = Vec::new();
    for id in &message_ids {
        let Some(message) = messages.get(id) else {
            items.push(Item::failure(id, ErrorCode::NotCached));
            continue;
        };
        let changes = match mailbox.changes(action, message) {
            Ok(changes) => changes,
            Err(code) => {
                items.push(Item::failure(id, code));
                continue;
            }
        };
        if changes.is_empty() {
            items.push(Item::success(id, "unchanged", Vec::new()));
            continue;
        }
        let ops = tx
            .organization_apply(changes.clone(), imap)
            .map_err(storage)?;
        items.push(Item::success(id, "updated", operation_ids(ctx, &ops)));
        changed.push(message.clone());
        forward.extend(changes);
    }
    let entry = if forward.is_empty() {
        None
    } else {
        let detail = format!("{} cached message(s)", changed.len());
        let reversal = tx.organization_reversal(&forward, labels, changed);
        Some(
            tx.organization_record(action.title(), &detail, &reversal)
                .map_err(storage)?,
        )
    };
    Ok(bulk_result(ctx, &items, entry))
}

fn label_of_kind(labels: &[Label], kind: LabelKind) -> Option<&Label> {
    labels.iter().find(|label| label.kind == kind)
}

fn target_for<'a>(
    action: Action,
    folder: Option<&'a Label>,
    labels: &'a [Label],
    imap: bool,
) -> Result<Option<&'a Label>, Failure> {
    let kind = match action {
        Action::Read | Action::Unread | Action::Star | Action::Unstar => return Ok(None),
        Action::AddLabel | Action::RemoveLabel => {
            if imap || folder.is_none_or(|label| label.kind != LabelKind::User) {
                return Err(unsupported("Label addition and removal require a user label on an account with local labels"));
            }
            return Ok(folder);
        }
        Action::Move => {
            let Some(folder) = folder else {
                return Err(invalid("Specify a destination folder"));
            };
            if !matches!(
                folder.kind,
                LabelKind::Inbox
                    | LabelKind::Archive
                    | LabelKind::Trash
                    | LabelKind::Spam
                    | LabelKind::User
            ) || (imap && folder.provider_id.is_none())
            {
                return Err(unsupported(
                    "This folder is unavailable as a move destination",
                ));
            }
            return Ok(Some(folder));
        }
        Action::Archive => LabelKind::Archive,
        Action::Trash => LabelKind::Trash,
        Action::Restore | Action::NotJunk => LabelKind::Inbox,
        Action::Junk => LabelKind::Spam,
    };
    if kind == LabelKind::Archive && !imap {
        return Ok(None);
    }
    let label = label_of_kind(labels, kind)
        .filter(|label| !imap || label.provider_id.is_some())
        .ok_or_else(|| unsupported("This account has no eligible cached folder for the action"))?;
    Ok(Some(label))
}

struct Mailbox<'a> {
    labels: &'a [Label],
    target: Option<&'a Label>,
    imap: bool,
}

impl Mailbox<'_> {
    fn changes(&self, action: Action, message: &Message) -> Result<Vec<LocalChange>, ErrorCode> {
        let ids = || vec![message.id.clone()];
        match action {
            Action::Read | Action::Unread => {
                let read = matches!(action, Action::Read);
                return Ok((read != message.is_read)
                    .then(|| LocalChange::SetRead {
                        message_ids: ids(),
                        read,
                    })
                    .into_iter()
                    .collect());
            }
            Action::Star | Action::Unstar => {
                let starred = matches!(action, Action::Star);
                return Ok((starred != message.is_starred)
                    .then(|| LocalChange::SetStarred {
                        message_ids: ids(),
                        starred,
                    })
                    .into_iter()
                    .collect());
            }
            _ => {}
        }
        let inbox = label_of_kind(self.labels, LabelKind::Inbox);
        let spam = label_of_kind(self.labels, LabelKind::Spam);
        if matches!(action, Action::NotJunk)
            && !spam.is_some_and(|label| message.has_label(&label.id))
        {
            return Ok(Vec::new());
        }
        if self.imap {
            return self.imap_move(message);
        }
        let mut change = LabelChanges::new(message);
        let trash = label_of_kind(self.labels, LabelKind::Trash);
        match action {
            Action::AddLabel => change.add(self.target),
            Action::RemoveLabel => change.remove(self.target),
            _ => match self
                .target
                .map(|label| label.kind)
                .unwrap_or(LabelKind::Archive)
            {
                LabelKind::Inbox => {
                    change.add(self.target);
                    change.remove(trash);
                    change.remove(spam);
                }
                LabelKind::Archive => {
                    change.remove(inbox);
                }
                LabelKind::Trash => {
                    // An explicit local label change avoids POP3's optional server DELE.
                    change.add(self.target);
                    change.remove(inbox);
                }
                LabelKind::Spam | LabelKind::User => {
                    if trash.is_some_and(|label| message.has_label(&label.id)) {
                        return Err(ErrorCode::Conflict);
                    }
                    change.add(self.target);
                    change.remove(inbox);
                }
                _ => return Err(ErrorCode::Unsupported),
            },
        }
        Ok(change.steps)
    }

    fn imap_move(&self, message: &Message) -> Result<Vec<LocalChange>, ErrorCode> {
        let target = self.target.ok_or(ErrorCode::Unsupported)?;
        let mut sources = self.labels.iter().filter(|label| {
            message.has_label(&label.id)
                && label.provider_id.is_some()
                && !label.kind.is_virtual()
                && label.kind != LabelKind::Container
        });
        let source = sources.next().ok_or(ErrorCode::Conflict)?;
        if sources.next().is_some() {
            return Err(ErrorCode::Conflict);
        }
        let mut change = LabelChanges::new(message);
        if source.id != target.id {
            // IMAP's AddLabel moves the server copy. A following RemoveLabel
            // clears the old cached membership in the same transaction.
            change.add(Some(target));
            change.remove(Some(source));
        }
        Ok(change.steps)
    }
}

struct LabelChanges<'a> {
    message: &'a Message,
    steps: Vec<LocalChange>,
}

impl<'a> LabelChanges<'a> {
    fn new(message: &'a Message) -> Self {
        Self {
            message,
            steps: Vec::new(),
        }
    }

    fn add(&mut self, label: Option<&Label>) {
        if let Some(label) = label.filter(|label| !self.message.has_label(&label.id)) {
            self.steps.push(LocalChange::AddLabel {
                message_ids: vec![self.message.id.clone()],
                label_id: label.id.clone(),
            });
        }
    }

    fn remove(&mut self, label: Option<&Label>) {
        if let Some(label) = label.filter(|label| self.message.has_label(&label.id)) {
            self.steps.push(LocalChange::RemoveLabel {
                message_ids: vec![self.message.id.clone()],
                label_id: label.id.clone(),
            });
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Focus {
    Focused,
    Other,
    Automatic,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FocusArgs {
    message_ids: Vec<String>,
    focus: Focus,
    #[serde(default)]
    train_senders: bool,
}

pub(super) fn focus(
    ctx: &WriteContext<'_>,
    tx: &McpWriteTxn<'_, '_>,
    input: FocusArgs,
) -> Result<Value, Failure> {
    let messages = select(ctx, tx, &input.message_ids)?;
    let labels = tx.organization_labels().map_err(storage)?;
    let inbox = label_of_kind(&labels, LabelKind::Inbox)
        .ok_or_else(|| unsupported("This account has no cached Inbox"))?;
    let excluded: Vec<_> = labels
        .iter()
        .filter(|label| matches!(label.kind, LabelKind::Spam | LabelKind::Trash))
        .collect();
    let focused = match input.focus {
        Focus::Focused => Some(true),
        Focus::Other => Some(false),
        Focus::Automatic => None,
    };
    let mut items = Vec::with_capacity(input.message_ids.len());
    let mut overrides = 0;
    let mut rules = 0;
    for id in &input.message_ids {
        let Some(message) = messages.get(id) else {
            items.push(Item::failure(id, ErrorCode::NotCached));
            continue;
        };
        if !message.has_label(&inbox.id)
            || excluded.iter().any(|label| message.has_label(&label.id))
        {
            items.push(Item::failure(id, ErrorCode::Conflict));
            continue;
        }
        let sender = if input.train_senders {
            let email = message.from.email.trim();
            if message.is_outgoing || email.is_empty() {
                items.push(Item::failure(id, ErrorCode::Conflict));
                continue;
            }
            Some(email)
        } else {
            None
        };
        let (override_changed, rule_changed) = tx
            .organization_focus(message, focused, sender)
            .map_err(storage)?;
        overrides += usize::from(override_changed);
        rules += usize::from(rule_changed);
        let status = if override_changed || rule_changed {
            "updated"
        } else {
            "unchanged"
        };
        items.push(Item::success(id, status, Vec::new()));
    }
    let entry = if overrides + rules > 0 {
        let detail = format!("{overrides} override(s), {rules} sender rule(s)");
        Some(
            tx.organization_record("Updated Focused Inbox", &detail, &Reversal::None)
                .map_err(storage)?,
        )
    } else {
        None
    };
    let mut result = bulk_result(ctx, &items, entry);
    result["overrides_changed"] = json!(overrides);
    result["sender_rules_changed"] = json!(rules);
    Ok(result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Delete {
    message_ids: Vec<String>,
    confirm: bool,
}

pub(super) fn delete(
    ctx: &WriteContext<'_>,
    tx: &McpWriteTxn<'_, '_>,
    input: Delete,
) -> Result<Value, Failure> {
    if !input.confirm {
        return Err(invalid(
            "Set confirm to true to permanently delete the exact selection",
        ));
    }
    if ctx.account.provider != ProviderKind::Sample {
        return Err(unsupported(
            "Exact permanent deletion is currently available for Sample accounts",
        ));
    }
    let messages = select(ctx, tx, &input.message_ids)?;
    if messages.len() != input.message_ids.len() {
        return Err(not_cached(
            "At least one target is unavailable in this account. No messages were deleted.",
        ));
    }
    let labels = tx.organization_labels().map_err(storage)?;
    let trash: &LabelId = &label_of_kind(&labels, LabelKind::Trash)
        .ok_or_else(|| unsupported("This account has no cached Trash folder"))?
        .id;
    if messages.values().any(|message| !message.has_label(trash)) {
        return Err(conflict(
            "Every target must be in Trash. No messages were deleted.",
        ));
    }
    let message_ids: Vec<MessageId> = input
        .message_ids
        .iter()
        .map(|id| messages[id].id.clone())
        .collect();
    let ops = tx
        .organization_apply(
            vec![LocalChange::DeleteForever {
                message_ids,
                folder: None,
            }],
            false,
        )
        .map_err(storage)?;
    let items: Vec<_> = input
        .message_ids
        .iter()
        .map(|id| Item::success(id, "deleted", operation_ids(ctx, &ops)))
        .collect();
    let detail = format!("{} cached message(s) in Trash", items.len());
    let entry = tx
        .organization_record("Deleted forever", &detail, &Reversal::None)
        .map_err(storage)?;
    Ok(bulk_result(ctx, &items, Some(entry)))
}

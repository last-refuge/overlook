//! Changes to the app-managed contact cache and its existing provider outbox.

#[cfg(test)]
mod tests;

use super::{parse, storage, ErrorCode, Failure, WriteContext, WritePermission, WriteTool};
use crate::mcp::read::{contacts::BookKey, eligible_url};
use overlook_core::{time::now, Address, Contact, OperationId, ProviderKind};
use overlook_store::McpWriteTxn;
use serde::Deserialize;
use serde_json::{json, Map, Value};

const CREATE: &str = "contacts_create";
const UPDATE: &str = "contacts_update";
const DELETE: &str = "contacts_delete";
const TEXT_LIMIT: usize = 320;
const NOTES_LIMIT: usize = 2048;

pub(super) fn tools() -> Vec<WriteTool> {
    let opaque = json!({"type":"string","minLength":1,"maxLength":128});
    let text = json!({"type":["string","null"],"maxLength":TEXT_LIMIT});
    let fields = json!({
        "given_name":text, "family_name":text, "phone":text, "organization":text, "title":text,
        "email":{"type":["string","null"],"maxLength":TEXT_LIMIT,"description":"Primary email address."},
        "notes":{"type":["string","null"],"maxLength":NOTES_LIMIT}
    });
    let mut create = fields.clone();
    create["address_book_id"] = opaque.clone();
    let target = json!({"contact_id":opaque,"if_revision":opaque});
    let mut update = fields;
    update
        .as_object_mut()
        .unwrap()
        .extend(target.as_object().unwrap().clone());
    let tool = |name, description, properties, required: &[&str]| {
        WriteTool::new(
            name,
            description,
            json!({"type":"object","properties":properties,"required":required}),
            WritePermission::Contacts,
        )
    };
    vec![
        tool(
            CREATE,
            "Create a contact in the account's default address book. Supply a name or an email address. For remote contacts, track operation_id and search the book again after sync for the provider-assigned contact ID.",
            create,
            &["address_book_id"],
        ),
        tool(
            UPDATE,
            "Update supplied contact fields using the current contacts_get revision. Omitted fields stay as they are. Use null to clear a field.",
            update,
            &["contact_id", "if_revision"],
        ),
        tool(
            DELETE,
            "Delete a contact using the current contacts_get revision.",
            target,
            &["contact_id", "if_revision"],
        ),
    ]
}

#[derive(Deserialize)]
struct Create {
    address_book_id: String,
}

#[derive(Deserialize)]
struct Target {
    contact_id: String,
    if_revision: String,
}

pub(super) fn execute(
    ctx: &WriteContext<'_>,
    tx: &mut McpWriteTxn<'_, '_>,
    name: &str,
    args: &Map<String, Value>,
) -> Result<Value, Failure> {
    if ctx.account.provider == ProviderKind::Gmail {
        return Err(Failure::new(
            ErrorCode::Unsupported,
            "Google contact writes are unavailable",
        ));
    }
    let connection = tx.contacts_connection().map_err(storage)?;
    if connection
        .as_ref()
        .is_some_and(|connection| !eligible_url(&connection.server_url))
    {
        return Err(Failure::new(
            ErrorCode::Unsupported,
            "This contacts connection is unavailable for MCP writes",
        ));
    }
    let connection = connection.map(|connection| (connection.credential, connection.default_book));
    let default = BookKey::default_for(ctx.account.provider, connection.as_ref());
    match name {
        CREATE => {
            let input: Create = parse(args)?;
            check_token(&input.address_book_id)?;
            let book = default
                .filter(|book| ctx.id("address_book", &book.raw_key()) == input.address_book_id)
                .ok_or_else(|| {
                    Failure::new(
                        ErrorCode::AccessDenied,
                        "Choose the account's known writable default address book",
                    )
                })?;
            let mut contact = Contact::new_local(tx.account().clone(), now());
            apply_fields(&mut contact, args)?;
            save(ctx, tx, &mut contact, book)
        }
        UPDATE | DELETE => {
            let target: Target = parse(args)?;
            check_token(&target.contact_id)?;
            check_token(&target.if_revision)?;
            let mut contact = tx
                .find_contact(|id| ctx.id("contact", id) == target.contact_id)
                .map_err(storage)?
                .ok_or_else(|| Failure::new(
                    ErrorCode::AccessDenied,
                    "Contact is unavailable for this account. Search the book again if the contact has synced.",
                ))?;
            let stored_book = tx.contact_book(&contact.id).map_err(storage)?;
            let book = BookKey::for_contact(
                ctx.account.provider,
                &contact,
                stored_book.as_deref().unwrap_or_default(),
                connection.as_ref(),
            );
            if !book.supports_write(default) {
                return Err(Failure::new(
                    ErrorCode::Unsupported,
                    "Contact is outside a known writable address book",
                ));
            }
            if ctx.contact_revision(&contact) != target.if_revision {
                return Err(Failure::new(
                    ErrorCode::Conflict,
                    "Contact revision has changed",
                ));
            }
            if name == DELETE {
                let operation = tx
                    .delete_mcp_contact(&contact, book.remote())
                    .map_err(storage)?;
                return Ok(result(ctx, &contact, book, operation.as_ref(), "deleted"));
            }
            let before = contact.clone();
            if !apply_fields(&mut contact, args)? {
                return Err(invalid("Supply at least one contact field to update"));
            }
            if contact == before {
                return Ok(result(ctx, &contact, book, None, "unchanged"));
            }
            save(ctx, tx, &mut contact, book)
        }
        _ => Err(invalid("Unknown contact write tool")),
    }
}

fn apply_fields(contact: &mut Contact, args: &Map<String, Value>) -> Result<bool, Failure> {
    let before_names = (contact.given_name.clone(), contact.family_name.clone());
    let mut supplied = false;
    for (name, target) in [
        ("given_name", &mut contact.given_name),
        ("family_name", &mut contact.family_name),
        ("phone", &mut contact.phone),
        ("organization", &mut contact.organization),
        ("title", &mut contact.title),
        ("notes", &mut contact.notes),
    ] {
        if let Some(value) = args.get(name) {
            *target = text_field(name, value)?;
            supplied = true;
        }
    }
    if before_names != (contact.given_name.clone(), contact.family_name.clone()) {
        let name = [
            contact.given_name.as_deref(),
            contact.family_name.as_deref(),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
        contact.name = (!name.is_empty()).then_some(name);
    }
    if let Some(value) = args.get("email") {
        let email = text_field("email", value)?
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !email.is_empty()
            && (email.contains(['<', '>', ',', ';']) || !Address::new(None, &email).looks_valid())
        {
            return Err(invalid("Invalid contact email address"));
        }
        if email != contact.email {
            contact.emails.retain(|e| {
                !e.eq_ignore_ascii_case(&contact.email) && !e.eq_ignore_ascii_case(&email)
            });
            if !email.is_empty() {
                contact.emails.insert(0, email.clone());
            }
            contact.email = email;
        }
        supplied = true;
    }
    if contact.known_name().is_none() && contact.email.is_empty() && contact.emails.is_empty() {
        return Err(invalid("Give the contact a name or an email address"));
    }
    Ok(supplied)
}

fn text_field(name: &str, value: &Value) -> Result<Option<String>, Failure> {
    if value.is_null() {
        return Ok(None);
    }
    let notes = name == "notes";
    let limit = if notes { NOTES_LIMIT } else { TEXT_LIMIT };
    let text = value
        .as_str()
        .filter(|text| {
            text.len() <= limit
                && text
                    .chars()
                    .all(|c| !c.is_control() || (notes && matches!(c, '\n' | '\r' | '\t')))
        })
        .ok_or_else(|| invalid(&format!("Invalid contact field {name}")))?;
    let text = text.trim();
    Ok((!text.is_empty()).then(|| text.to_owned()))
}

fn save(
    ctx: &WriteContext<'_>,
    tx: &McpWriteTxn<'_, '_>,
    contact: &mut Contact,
    book: BookKey<'_>,
) -> Result<Value, Failure> {
    let operation = tx
        .save_mcp_contact(contact, book.remote(), book.carddav_book())
        .map_err(storage)?;
    Ok(result(ctx, contact, book, operation.as_ref(), "saved"))
}

fn result(
    ctx: &WriteContext<'_>,
    contact: &Contact,
    book: BookKey<'_>,
    operation: Option<&OperationId>,
    action: &str,
) -> Value {
    let mut value = json!({
        "contact_id": ctx.id("contact", &contact.id),
        "address_book_id": ctx.id("address_book", &book.raw_key()),
        "operation_id": operation.map(|id| ctx.id("operation", id.as_str())),
        "status": if operation.is_some() { "queued" } else { action },
    });
    if action != "deleted" {
        value["revision"] = json!(ctx.contact_revision(contact));
    }
    value
}

fn check_token(token: &str) -> Result<(), Failure> {
    if token.is_empty() || token.len() > 128 {
        return Err(invalid("Invalid contact reference or revision"));
    }
    Ok(())
}

fn invalid(message: &str) -> Failure {
    Failure::new(ErrorCode::InvalidRequest, message)
}

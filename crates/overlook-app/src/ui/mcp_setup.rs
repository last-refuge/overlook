//! Account grants for local MCP clients.

use std::collections::BTreeSet;

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Disableable, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::*;
use overlook_core::{Account, AccountId};
use overlook_store::Store;

use super::{section, setting_row, MailWindow};
use crate::mcp::{self, ClientGrant, WritePermission};
use crate::theme::ActivePalette;

const PERMISSIONS: [(WritePermission, &str); 6] = [
    (WritePermission::Modify, "Organize mail and edit drafts"),
    (WritePermission::Send, "Send mail"),
    (WritePermission::PermanentDelete, "Permanently delete mail"),
    (WritePermission::Calendar, "Edit calendar events"),
    (WritePermission::Invite, "Manage invitations"),
    (WritePermission::Contacts, "Edit contacts"),
];

pub(super) struct McpSetup {
    client: Entity<InputState>,
    accounts: Vec<Account>,
    selected: BTreeSet<AccountId>,
    permissions: BTreeSet<WritePermission>,
    grants: Vec<ClientGrant>,
    issued: Option<IssuedGrant>,
    note: Option<String>,
}

struct IssuedGrant {
    client: String,
    token: Option<String>,
}

impl McpSetup {
    pub(super) fn new(store: &Store, window: &mut Window, cx: &mut Context<MailWindow>) -> Self {
        let client = cx.new(|cx| InputState::new(window, cx).placeholder("my-harness"));
        let accounts = mcp::eligible_accounts(store);
        let mut setup = Self {
            client,
            accounts: accounts.unwrap_or_default(),
            selected: BTreeSet::new(),
            permissions: BTreeSet::new(),
            grants: Vec::new(),
            issued: None,
            note: None,
        };
        setup.refresh_grants();
        setup
    }

    fn refresh_grants(&mut self) {
        match mcp::client_grants(&crate::data_dir()) {
            Ok(grants) => self.grants = grants,
            Err(_) => self.note = Some("Could not read this profile’s client grants.".into()),
        }
    }
}

impl MailWindow {
    fn mcp_setup_mut(&mut self) -> Option<&mut McpSetup> {
        self.settings.as_mut()?.mcp.as_mut()
    }

    fn issue_mcp_grant(&mut self, cx: &mut Context<Self>) {
        let Some(setup) = self.mcp_setup_mut() else {
            return;
        };
        let client = setup.client.read(cx).value().trim().to_owned();
        let accounts = setup
            .selected
            .iter()
            .map(|id| id.0.clone())
            .collect::<Vec<_>>();
        match mcp::grant_client(
            &crate::data_dir(),
            &client,
            &accounts,
            setup.permissions.clone(),
        ) {
            Ok(token) => {
                setup.issued = Some(IssuedGrant {
                    client,
                    token: Some(token),
                });
                setup.note = None;
                setup.refresh_grants();
            }
            Err(error) => setup.note = Some(error.to_string()),
        }
        cx.notify();
    }

    fn revoke_mcp_grant(&mut self, client: Option<String>, cx: &mut Context<Self>) {
        let Some(setup) = self.mcp_setup_mut() else {
            return;
        };
        let client = client.unwrap_or_else(|| setup.client.read(cx).value().trim().to_owned());
        match mcp::revoke_client(&crate::data_dir(), &client) {
            Ok(()) => {
                if setup
                    .issued
                    .as_ref()
                    .is_some_and(|grant| grant.client == client)
                {
                    setup.issued = None;
                }
                setup.note = Some(format!("Access revoked for {client}."));
                setup.refresh_grants();
            }
            Err(error) => setup.note = Some(error.to_string()),
        }
        cx.notify();
    }

    pub(super) fn render_mcp_setup(&self, setup: &McpSetup, cx: &mut Context<Self>) -> AnyElement {
        let palette = *cx.palette();
        let detail = |text: &str| {
            div()
                .text_size(crate::ui::ui_px(12.))
                .text_color(palette.muted)
                .child(text.to_owned())
        };
        let label = |text: &str| {
            div()
                .text_size(crate::ui::ui_px(13.))
                .font_weight(FontWeight::MEDIUM)
                .child(text.to_owned())
        };
        let copy_controls = setup.issued.as_ref().map(|grant| {
            let has_token = grant.token.is_some();
            v_flex().gap(px(7.)).rounded(px(8.)).border_1().border_color(palette.divider).p(px(12.))
                .child(label(&format!("Set up {}", grant.client)))
                .child(detail(if has_token { "Copy the token before leaving this page. Save it in your harness’s secret storage." } else { "Put the copied token in OVERLOOK_MCP_TOKEN in your harness." }))
                .child(h_flex().gap(px(8.))
                    .child(Button::new("mcp-copy-setup").small().outline().label("Copy stdio setup")
                        .on_click(cx.listener(|this, _, _, cx| {
                            let Some(setup) = this.mcp_setup_mut() else { return };
                            let Some(grant) = setup.issued.as_ref() else { return };
                            match harness_config(&grant.client) {
                                Ok(config) => cx.write_to_clipboard(ClipboardItem::new_string(config)),
                                Err(_) => setup.note = Some("Could not locate Overlook’s executable or profile.".into()),
                            }
                            cx.notify();
                        })))
                    .child(Button::new("mcp-copy-token").small().primary().label("Copy token").disabled(!has_token)
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(token) = this.mcp_setup_mut().and_then(|setup| setup.issued.as_mut()).and_then(|grant| grant.token.take()) {
                                cx.write_to_clipboard(ClipboardItem::new_string(token));
                                cx.notify();
                            }
                        }))))
        });

        v_flex().gap(px(12.))
            .child(section("MCP", Some("Choose which accounts a local AI harness can access. New grants have read access until you add permissions.")))
            .child(setting_row("Client name", "Use the same name in the harness. Issuing a token again replaces its access.", Input::new(&setup.client).small().into_any_element()))
            .child(label("Accounts"))
            .when(setup.accounts.is_empty(), |d| d.child(detail("Add an eligible account before issuing a grant.")))
            .children(setup.accounts.iter().map(|account| {
                let id = account.id.clone();
                setting_row(&account.display_name, &account.email,
                    Switch::new(SharedString::from(format!("mcp-account-{}", id.0)))
                        .checked(setup.selected.contains(&id))
                        .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                            if let Some(setup) = this.mcp_setup_mut() {
                                if *checked { setup.selected.insert(id.clone()); } else { setup.selected.remove(&id); }
                                cx.notify();
                            }
                        })).into_any_element())
            }))
            .child(label("Write permissions"))
            .children(PERMISSIONS.iter().map(|&(permission, description)| {
                setting_row(description, "", Switch::new(SharedString::from(format!("mcp-permission-{permission:?}")))
                    .checked(setup.permissions.contains(&permission))
                    .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                        if let Some(setup) = this.mcp_setup_mut() {
                            if *checked { setup.permissions.insert(permission); } else { setup.permissions.remove(&permission); }
                            cx.notify();
                        }
                    })).into_any_element())
            }))
            .child(h_flex().gap(px(8.))
                .child(Button::new("mcp-issue").small().primary().label("Issue token").disabled(setup.selected.is_empty())
                    .on_click(cx.listener(|this, _, _, cx| this.issue_mcp_grant(cx))))
                .child(Button::new("mcp-revoke-name").small().outline().label("Revoke by name")
                    .on_click(cx.listener(|this, _, _, cx| this.revoke_mcp_grant(None, cx)))))
            .children(setup.note.as_deref().map(detail))
            .children(copy_controls)
            .child(section("MCP clients", None))
            .when(setup.grants.is_empty(), |d| d.child(detail("No named client grants for this profile.")))
            .children(setup.grants.iter().map(|grant| {
                let client = grant.client.clone();
                let mut summary = grant.account_ids.iter().map(|id| {
                    setup.accounts.iter().find(|account| &account.id.0 == id).map_or(id.as_str(), |account| account.email.as_str())
                }).collect::<Vec<_>>().join(", ");
                for (permission, label) in PERMISSIONS {
                    if grant.permissions.contains(&permission) {
                        summary.push_str(" · ");
                        summary.push_str(label);
                    }
                }
                setting_row(&grant.client, &summary,
                    Button::new(SharedString::from(format!("mcp-revoke-{client}"))).small().danger().label("Revoke")
                        .on_click(cx.listener(move |this, _, _, cx| this.revoke_mcp_grant(Some(client.clone()), cx)))
                        .into_any_element())
            }))
            .into_any_element()
    }
}

fn harness_config(client: &str) -> anyhow::Result<String> {
    let binary = std::env::current_exe()?;
    let profile = crate::data_dir().canonicalize()?;
    Ok(serde_json::to_string_pretty(&serde_json::json!({
        "command": binary,
        "args": ["mcp", "serve", "--client", client],
        "env": {"OVERLOOK_DATA_DIR": profile},
    }))?)
}

# Connect an AI harness to Overlook

Overlook serves one local profile at a time. An MCP client uses its own grant to read or change only the accounts and capabilities you select. The same grant works while the desktop owns the profile and while the foreground headless owner runs.

## Set up stdio

1. Open **Settings → Integrations** in Overlook. Set a client name, select accounts, and issue a token. Read access is included. Add only the write permissions this client needs.
2. Select **Copy stdio setup** for the executable, arguments, and profile environment. Select **Copy token** and save the token under `OVERLOOK_MCP_TOKEN` in the harness’s secret storage.
3. Start the harness and run `accounts_list`. Use `account_capabilities` to check the selected account’s coverage and granted permissions.

The installed Linux executable is typically `~/.local/bin/overlook` or `/usr/bin/overlook` on Arch. On macOS launch the executable inside `Overlook.app/Contents/MacOS/overlook`, typically in `~/Applications` or `/Applications`. Give the harness an absolute path, including when the path contains spaces. Use the matching profile directory in `OVERLOOK_DATA_DIR`. An example server entry for a harness that takes a command, argv, and environment is shown below. Add the credential separately through the harness.

```json
{
  "command": "/absolute/path/to/overlook",
  "args": ["mcp", "serve", "--client", "my-harness"],
  "env": {
    "OVERLOOK_DATA_DIR": "/absolute/path/to/profile"
  }
}
```

To issue a grant from a terminal, set `OVERLOOK_DATA_DIR` to the same profile and run the following command. Substitute the account ID shown here with your own. Repeat `--account-id` for multiple accounts. Repeat `--allow` for each write permission you need. The CLI returns the token once.

```sh
overlook mcp grant --client my-harness --account-id 'imap:me@example.test' --allow modify
```

Eligible IMAP and POP3 account IDs use `imap:<address>` or `pop3:<address>`. The fictional account IDs are `sample` and `sample-secondary`.

To remove access, select **Revoke** in Settings or run `overlook mcp revoke --client my-harness` with the same profile. Older grants can be revoked by name. Revocation applies to the next tool call, including on an open connection. Issuing a grant again for the same client rotates its token and replaces its account list and permissions.

## Keep a profile available with the desktop closed

Run `overlook mcp headless` in a terminal under the same OS login and profile. The headless process uses Overlook’s store, provider credentials, and write queues. On macOS or Linux, keep the OS credential store available to the owner to sync connected accounts.

The desktop and headless processes take turns owning the profile. A foreground desktop launch waits up to 30 seconds for the headless owner to finish its current work and release the profile. If the wait expires, the profile stays locked until that work finishes. The stdio bridge uses the new owner for subsequent calls. A call during the transfer can return `profile_unavailable`. Keep the same `idempotency_key` when retrying a write with identical inputs.

Run `overlook --quit` with the same profile to stop the active owner. Start headless again if you want to keep using the profile after quitting the desktop. A running desktop already serves MCP, and repeated owner starts preserve the existing owner.

When upgrading, quit any older desktop process before starting the new binary. Use the same Overlook version for the desktop and headless commands so both participate in the profile lock.

## Try fictional data

In a terminal, create a disposable profile and start its sample owner. Pass the printed path to the harness and any other terminal used for the test.

```sh
export OVERLOOK_DATA_DIR="$(mktemp -d)"
printf '%s\n' "$OVERLOOK_DATA_DIR"
overlook mcp sample --offline
```

In a second terminal with that profile selected, grant `sample` and `sample-secondary` to a client. From the harness, find the **Trail walk** invitation in the primary Sample mailbox, check its event in the secondary Sample calendar, and look up Sophie in the secondary Sample contacts. Shut down the sample process with `overlook --quit` when done. A normal `mcp sample` start on the same directory replays queued fictional operations.

## Supported operations

| Permission | Available operations |
| --- | --- |
| Read | Discover accounts, search cached mail, calendar and contacts, read cached messages and eligible attachments, inspect drafts and Outbox, and check cache coverage. Sample and IMAP support explicit provider mail search and refresh. |
| `modify` | Organize mail, train Focused Inbox, undo supported changes, edit drafts and attachments, and manage top-level offline POP3 folders. |
| `send` | Queue confirmed sends and cancel queued sends before submission. |
| `permanent-delete` | Permanently delete explicitly selected Sample messages in Trash, with confirmation. |
| `calendar` | Personal event changes on writable Sample and CalDAV calendars, supported CalDAV recurrence scopes, and CalDAV conflict resolution. |
| `invite` | Confirmed RSVP and guest-list changes in the Sample profile with simulated delivery. |
| `contacts` | Create, update, and delete app-managed and Sample contacts, Microsoft Graph default-book contacts, and supported CardDAV default-book contacts. |

Read-only collections and inferred contacts have restricted write support. Gmail and recognized Google endpoints are gated pending the Google data review. See the [MCP implementation plan](../llm-docs/overlook-mcp-plan.md) for individual tools, provider limits, and the write receipt contract.

## Optional Streamable HTTP deployment

Start `overlook mcp serve-http --config /private/path/overlook-http.json` only when your harness needs HTTP. Start an Overlook owner for the same profile first. The HTTP process listens on numeric loopback and defaults to `127.0.0.1:8765`. Use `--bind` to change the loopback address or port.

For a local HTTP harness that can send a bearer token, make the configuration file owner-only with `chmod 600`. `grant_token` is the credential Overlook issued for the selected `grant_client`. `bearer_token` is a separate, random token for the HTTP client. Use 32 random bytes encoded as unpadded base64url. Keep both credentials in private storage.

```json
{
  "grant_client": "my-harness",
  "grant_token": "<Overlook client grant token>",
  "auth": {
    "mode": "loopback",
    "bearer_token": "<separate HTTP token>"
  }
}
```

Connect to `http://127.0.0.1:8765/mcp` and send `Authorization: Bearer <HTTP token>` on each request. Loopback mode expects `Host` to match the numeric bind and is for direct local connections. Set `allowed_origins` only when a known harness supplies an `Origin` header. Use exact origins. Without this list the bridge accepts requests that omit `Origin`. Browser CORS preflight is unavailable.

For a remote deployment, configure an HTTPS reverse proxy on the same host and an external OAuth authorization server. Protect the numeric loopback upstream and preserve the canonical public `Host`, `Origin`, `Authorization`, and both routes below. Enable this deployment deliberately using the `tls_proxy` authentication mode.

```json
{
  "grant_client": "my-remote-harness",
  "grant_token": "<Overlook client grant token>",
  "auth": {
    "mode": "tls_proxy",
    "resource": "https://mcp.example.test/mcp",
    "issuer": "https://identity.example.test",
    "introspection_url": "https://identity.example.test/introspect",
    "introspection_bearer": "<authorization server introspection credential>",
    "subject": "<exact allowed OAuth subject>"
  }
}
```

Forward `/mcp` and `/.well-known/oauth-protected-resource/mcp`, including any public port in `Host`. Configure discovery and client registration on the authorization server. It must issue access tokens for this exact `resource` as the sole audience and return active status, issuer, audience, subject, expiry, and scopes through introspection. Discovery advertises `overlook:mcp` for basic access. Write tools and `mail_refresh` require `overlook:mcp:write` as well. A client can request both when configured for writes or use the scopes in the bridge’s `403` authorization challenge to request a write-capable token. The local grant still controls account and tool permissions. Keep the HTTP configuration file and owner profile private.

The bridge returns sessionless JSON for POST requests. Authenticated GET and DELETE requests to `/mcp` return `405`. Send both `application/json` and `text/event-stream` in `Accept`. Modern requests carry the protocol version, method, and applicable tool name in the matching MCP headers. Older clients may omit the protocol version on `initialize` and send `2025-11-25` on subsequent requests. [Streamable HTTP](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/streamable-http) · [MCP OAuth requirements](https://modelcontextprotocol.io/specification/2026-07-28/basic/authorization)

## Verification

Run the workspace tests and verify the exact package you will distribute. The package check installs into a temporary path containing spaces and exercises the staged executable with an isolated Sample profile.

```sh
cargo test --workspace --locked
python3 scripts/check-mcp-package.py /path/to/overlook-archive
```

To qualify a live provider, use a dedicated account and separate Overlook profile that contain only test records. Give one harness a scoped grant. Start with account discovery, cached reads, and explicit provider refresh. For each supported write, check the receipt, inspect provider state, and retry the same request with the original idempotency key. Use a fictional recipient controlled by the test account owner for send tests. Record the provider, capability, and result separately from sample and loopback checks.

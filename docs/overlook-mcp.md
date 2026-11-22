# Set up Overlook MCP

Overlook's Model Context Protocol (MCP) server lets an AI client work with mail, calendar, and
contacts in an Overlook profile. Each client gets its own token and can access only the accounts
and write permissions you select. For a client running on the same machine, use **stdio**. The
client launches a small bridge that talks to the running Overlook desktop or headless process.

[Local setup](#set-up-a-local-client) · [Fictional data](#try-fictional-data) ·
[Headless mode](#keep-a-profile-available-with-the-desktop-closed) ·
[Streamable HTTP](#optional-streamable-http-deployment) · [Troubleshooting](#troubleshooting)

## Set up a local client

### 1. Check your Overlook installation

Install Overlook on [Linux or macOS](../README.md#install). Use the absolute path to the executable
in your client's configuration. Common locations are `~/.local/bin/overlook` for the Linux tarball,
`/usr/bin/overlook` on Arch, and `~/Applications/Overlook.app/Contents/MacOS/overlook` on macOS.
If the app is installed system-wide on macOS, look in `/Applications`.

Run the following in a terminal. Replace the path with your own and keep the quotes around it.

```sh
OVERLOOK_BIN="/absolute/path/to/overlook"
"$OVERLOOK_BIN" mcp --help
```

The output should start with `Usage: overlook mcp <command>` and list `serve`. MCP is available in
Overlook 2.1.0 and later. If you see the general Overlook help, update Overlook or build from current
`main` using the [Linux](../README.md#ubuntu-and-other-linux-source-builds) or
[macOS](../README.md#build-from-source) instructions.

Open Overlook and keep it running under the same OS login as your MCP client. For a live account,
add it in **Settings → Accounts** first. MCP supports eligible IMAP and POP3 accounts and the
fictional Sample accounts. Gmail and recognized Google endpoints are gated pending Google's data
review. To test without connecting an account, [start an isolated sample profile](#try-fictional-data).

### 2. Create a grant

1. In Overlook, open **Settings → Integrations**. Under **MCP**, enter a client name such as
   `my-harness`. Use a different name for each client you want to manage independently.
2. Turn on the accounts the client should be able to see. Read access is included. Leave the
   write switches off for the first test, then add [write permissions](#supported-operations) as needed.
3. Select **Issue token**, then **Copy stdio setup**. Paste the setup into your client or use it
   as a reference for the examples below. It contains the exact executable path, the client name,
   and the profile path in `OVERLOOK_DATA_DIR`.
4. Select **Copy token** before leaving the page. Save it in your client's secret store or a
   password manager. The token can be copied once. Supply it to the bridge as `OVERLOOK_MCP_TOKEN`.

### 3. Configure the client

Use the copied `command`, `args`, and `env` values. Expand `~` to the full home directory in paths,
including the profile path. Put the whole executable path in `command`, including any spaces, and
keep each argument in its own array entry. The value after `--client` must match the grant name.
`OVERLOOK_DATA_DIR` must point to the profile that issued the grant.

#### JSON configuration

If your client uses a top-level `mcpServers` object, add the copied entry under a name such as
`overlook`. Merge it into any existing entries. The entry name is how the server shows up in the
client. `my-harness` is the grant name that Overlook checks.

```json
{
  "mcpServers": {
    "overlook": {
      "command": "/absolute/path/to/overlook",
      "args": ["mcp", "serve", "--client", "my-harness"],
      "env": {
        "OVERLOOK_DATA_DIR": "/absolute/path/to/profile"
      }
    }
  }
}
```

Use the client's secret or environment settings to pass `OVERLOOK_MCP_TOKEN`. If the client accepts
credentials only through the `env` object, add `OVERLOOK_MCP_TOKEN` there and keep the configuration
file private, with owner-only permissions (`chmod 600`). Keep the token out of shared project settings.
Restart the client after saving the entry.

#### Codex CLI

Add this to `~/.codex/config.toml` and substitute the paths and grant name from **Copy stdio setup**.
`env_vars` forwards the token from the environment that launches Codex. [Codex MCP documentation](https://learn.chatgpt.com/docs/extend/mcp?surface=cli#stdio-servers)

```toml
[mcp_servers.overlook]
command = "/absolute/path/to/overlook"
args = ["mcp", "serve", "--client", "my-harness"]
env_vars = ["OVERLOOK_MCP_TOKEN"]

[mcp_servers.overlook.env]
OVERLOOK_DATA_DIR = "/absolute/path/to/profile"
```

From Bash, paste the grant token at the silent prompt and press Enter. Start Codex from that shell
so the bridge receives the exported token. In Codex, use `/mcp` to see the server.

```bash
read -r -s -p 'Overlook MCP token: ' OVERLOOK_MCP_TOKEN
printf '\n'
export OVERLOOK_MCP_TOKEN
codex
```

#### Claude Code

Run this from the project where you want the server available. The single-quoted argument stores
the variable reference in your local Claude Code configuration. Claude Code resolves it from the
shell when it runs. [Claude Code MCP documentation](https://code.claude.com/docs/en/mcp#option-3-add-a-local-stdio-server)

```bash
claude mcp add --scope local \
  --env 'OVERLOOK_MCP_TOKEN=${OVERLOOK_MCP_TOKEN}' \
  --env 'OVERLOOK_DATA_DIR=/absolute/path/to/profile' \
  --transport stdio overlook -- "/absolute/path/to/overlook" mcp serve --client my-harness
```

Substitute your paths and grant name, then export the token using the same Bash prompt shown
above. Run `claude` from that shell and use `/mcp` to see the server.

### 4. Verify access

Ask the client to use Overlook's `accounts_list` tool and list the accounts it can see. Compare
the results with the accounts selected in the grant. Then call `account_capabilities` with an
`account_id` returned by `accounts_list`. It shows supported domains, cache status, and granted
write permissions. Use the returned ID in tool calls. The IDs used with `mcp grant`, such as
`imap:me@example.test`, are for grant setup.

A client can discover the tools before the bridge has authenticated to the profile. A successful
`accounts_list` call verifies the profile connection and grant.

## Try fictional data

The sample owner creates two fictional accounts in a separate profile. In one terminal, set
`OVERLOOK_BIN` to your executable and start the owner. Keep this terminal open.

```sh
OVERLOOK_BIN="/absolute/path/to/overlook"
SAMPLE_PROFILE="$(mktemp -d)"
printf 'Sample profile: %s\n' "$SAMPLE_PROFILE"
OVERLOOK_DATA_DIR="$SAMPLE_PROFILE" "$OVERLOOK_BIN" mcp sample --offline
```

After `Overlook MCP sample profile is ready.` appears, open a second terminal. Use the printed path
and create a read-only grant for both sample accounts. This captures the token in the shell without
printing it.

```sh
OVERLOOK_BIN="/absolute/path/to/overlook"
export OVERLOOK_DATA_DIR="/path/printed/by/the/first/terminal"
OVERLOOK_MCP_TOKEN=$(
  "$OVERLOOK_BIN" mcp grant --client overlook-sample \
    --account-id sample --account-id sample-secondary
)
export OVERLOOK_MCP_TOKEN
```

Follow [the client configuration steps](#3-configure-the-client) with `overlook-sample` as the grant
name and the printed path as `OVERLOOK_DATA_DIR`. Start the client from the second terminal if it
reads the token from the shell. Call `accounts_list` and expect two accounts with provider `sample`.
For a read-only test, ask the client to find the **Trail walk** invitation in the primary Sample
mailbox and **Sophie Williams** in the secondary Sample contacts.

When you're finished, run `"$OVERLOOK_BIN" --quit` from the second terminal, where the sample profile
is still selected. To replay queued fictional operations, restart the sample owner with `mcp sample`
and the same profile path. If you return to your everyday profile from the same shell, clear the sample
variables with `unset OVERLOOK_DATA_DIR OVERLOOK_MCP_TOKEN` first.

## Keep a profile available with the desktop closed

Quit the desktop, then run the following command under the same OS login. Use the executable and
profile paths from **Copy stdio setup**. Keep this terminal open while the client needs the profile.

```sh
OVERLOOK_DATA_DIR="/absolute/path/to/profile" "/absolute/path/to/overlook" mcp headless
```

Wait for `Overlook MCP headless profile is ready.` and use the same client configuration and grant.
The headless process uses Overlook's local store, provider credentials, and write queues. Keep Apple
Keychain on macOS or the Secret Service keyring on Linux available so it can sync connected accounts.

When you open the desktop again, it waits up to 30 seconds for the headless process to finish its
current work and release the profile. If the wait expires, the profile stays locked until that work
finishes. The stdio bridge uses the desktop for subsequent calls once it takes ownership. A call
during the transfer can return `profile_unavailable`. Keep the same `idempotency_key` when retrying
a write with identical inputs.

Use `Ctrl+C` in the headless terminal or run `overlook --quit` with the same profile to stop the active
owner. Start headless again if you want to keep using the profile after quitting the desktop. A running
desktop already serves MCP and keeps ownership if you try to start headless for the same profile.

When upgrading, quit any older desktop process before starting the new binary. Use the same Overlook
version for the desktop and headless commands so both participate in the profile lock.

## Manage grants from the terminal

Use the same executable and profile paths as the running owner. Substitute your account ID for
`imap:me@example.test`. Eligible live IDs use `imap:<address>` or `pop3:<address>`. Repeat
`--account-id` to include additional accounts and `--allow` for each write permission you need.
Omit `--allow` for read-only access. The command emits the token once on standard output.

```sh
OVERLOOK_BIN="/absolute/path/to/overlook"
export OVERLOOK_DATA_DIR="/absolute/path/to/profile"
"$OVERLOOK_BIN" mcp grant --client my-harness --account-id 'imap:me@example.test' --allow modify
```

Issuing a grant again for the same client replaces its account list and permissions and invalidates
the previous token. Update the credential in the client and restart it after rotation. To remove
access, select **Revoke** next to the client under **MCP clients** or run the following with the same
profile selected. Revocation takes effect on the next tool call, including on an open connection.

```sh
"$OVERLOOK_BIN" mcp revoke --client my-harness
```

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

Write support depends on the provider and collection. Check `account_capabilities` and the
[MCP implementation plan](../llm-docs/overlook-mcp-plan.md) for provider limits, individual tools,
and the write receipt contract.

## Optional Streamable HTTP deployment

Use Streamable HTTP for clients that need a URL. The HTTP bridge needs a running Overlook owner
and a grant for that profile, just like stdio. Its default listen address is `127.0.0.1:8765`.
Use `--bind` to choose a different numeric loopback address or port.

### Local HTTP with a bearer token

Create a grant for the HTTP bridge, then save the following JSON to a private configuration file.
`grant_token` is the Overlook credential for `grant_client`. Generate a separate `bearer_token` with
`python3 -c 'import secrets; print(secrets.token_urlsafe(32))'` for the HTTP client.

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

With the desktop, headless, or sample owner running, start the bridge in a separate terminal. Use
the same profile path as the owner and the path where you saved the JSON.

```sh
chmod 600 "/private/path/overlook-http.json"
OVERLOOK_DATA_DIR="/absolute/path/to/profile" "/absolute/path/to/overlook" \
  mcp serve-http --config "/private/path/overlook-http.json"
```

In the client, select **Streamable HTTP**, set the URL to `http://127.0.0.1:8765/mcp`, and send
`Authorization: Bearer <HTTP token>` on each request. Run `accounts_list` to verify access.
Loopback mode expects `Host` to match the numeric bind. If the client sends an `Origin` header,
set `allowed_origins` in the configuration to the exact allowed origins. Browser CORS preflight
is unavailable.

### Remote HTTP behind a TLS proxy

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

## Troubleshooting

| Symptom | What to check |
| --- | --- |
| No **Integrations** page or the bridge exits immediately | Run `mcp --help` against the exact `command` path from your client setup. Check that it points to a current Overlook executable and that each argument has its own entry. |
| `profile_unavailable` | Start the desktop, headless, or sample owner under the same OS login and check that `OVERLOOK_DATA_DIR` matches its profile. If the desktop is taking over from headless, wait for it to finish opening and retry. |
| `access_denied` | Check that the bridge received `OVERLOOK_MCP_TOKEN`, that `--client` matches the grant name, and that the grant is still active. For an individual tool call, check its account ID and required write permission. |
| `accounts_list` is empty or an account is missing | Check the profile, the accounts selected in the grant, and the account's eligibility. The CLI can issue a grant before the account exists, but tools only return eligible accounts present in that profile. |
| A read is stale or returns `not_cached` | Check cache status with `account_capabilities`. Let Overlook sync the profile. Sample and eligible IMAP accounts also offer `mail_refresh` for mail. |
| `overlook mcp headless` says Overlook is already running | The desktop can keep running in the background after you close its window. Quit Overlook, including any tray instance, and start headless again. |
| HTTP returns `401`, `403`, or `421` | Check the bearer credential or OAuth scopes, the exact request `Host`, and any `Origin` header against `allowed_origins`. |

## Maintainer verification

Run the workspace tests and verify the exact package you will distribute. The package check installs into a temporary path containing spaces and exercises the staged executable with an isolated Sample profile.

```sh
cargo test --workspace --locked
python3 scripts/check-mcp-package.py /path/to/overlook-archive
```

To qualify a live provider, use a dedicated account and separate Overlook profile that contain only test records. Give one harness a scoped grant. Start with account discovery, cached reads, and explicit provider refresh. For each supported write, check the receipt, inspect provider state, and retry the same request with the original idempotency key. Use a fictional recipient controlled by the test account owner for send tests. Record the provider, capability, and result separately from sample and loopback checks.

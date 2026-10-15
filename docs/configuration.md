# Configuration and S3

[English](configuration.md) | [简体中文](configuration.zh-CN.md)

## Daemon configuration

Run `agentfsd init --directory /absolute/path/to/agentfs` to create `agentfs.json`, a private token file, and the mount/exchange directories. Edit the generated configuration, then start `agentfsd serve --config /absolute/path/to/agentfs/agentfs.json`. The daemon loads the file at startup and checks it every second for runtime changes.

The generated file contains `data_dir`, `listen`, `mount_roots`, `directory_roots`, and `credentials`. Optional settings use the defaults below and can be added when needed.

| Field | Meaning and default |
| --- | --- |
| `data_dir` | Required absolute path for local state; initialization selects `<directory>/state`. One daemon per state directory. |
| `listen` | Required loopback socket address; initialization selects `127.0.0.1:7421`. |
| `mount_roots` | Required array of allowed absolute mount roots; initialization creates `<directory>/mounts`. |
| `directory_roots` | Allowed absolute import/export roots; omitted means `[]`. Initialization creates and permits `<directory>/exchange`. |
| `credentials` | Required array of `{ "principal": "owner", "token_file": "/absolute/path/token" }`. At least one credential is required. |
| `administrators` | Principals allowed to read and change daemon settings, default `["owner"]`. Independent of workspace grants; changes require a restart. |
| `allowed_origins` | HTTP Origin allowlist, default `[]`. CLI and stdio connections do not send an Origin. |
| `cache` | Optional object; omitted fields default to `max_bytes: 2147483648`, `target_bytes: 1610612736`, `min_free_bytes: 67108864`. Override individual fields as needed; the resulting `target_bytes` must be below `max_bytes`. |
| `validation` | Map of named container checks, default `{}`; see [operations](operations.md). |
| `docker` | Docker executable name or path, default `docker`. |
| `remote` | S3 configuration below. Omitted or `null` means local storage only. |

For example, adding `"cache": { "max_bytes": 3221225472 }` sets the cache limit to 3 GiB while retaining the default target size and free-space threshold. When selecting a limit at or below the default target, also supply a smaller `target_bytes`.

All state, root, and token paths must be absolute. Create allowed roots before starting the daemon. JSON strings do not expand `~`, `$HOME`, or environment variables. Windows paths use escaped backslashes, such as `"C:\\Users\\alice\\agentfs\\state"`, or forward slashes. Unknown top-level configuration fields and unknown `cache`, `validation` entry or `remote` fields are rejected. The configuration file is limited to 1 MiB.

Token files must be regular files, at most 4096 bytes, containing at least 32 non-whitespace bytes after trimming. On Unix, group and other permissions must be absent; initialization creates mode `0600`. Tokens must be unique. A token selects the configured `principal`; workspace ownership and grants determine that principal's access. Protect configuration and state using the daemon user's filesystem permissions.

## Runtime updates and dashboard settings

Open **Settings** in the dashboard sidebar, or visit `/settings`. Authenticate with an administrator token. The form edits cache sizes in MiB, S3 connections and credentials, named container validation checks, and allowed HTTP origins. Saved S3 connection changes are applied while the daemon runs. Saving persists the changes to the same `agentfs.json` used to start the daemon and publishes one configuration snapshot. The configuration retains its existing top-level structure; optional defaults remain omitted when saving.

| Configuration | Effective time |
| --- | --- |
| `remote` | After preparing history and the destination connection, without restarting |
| `cache` | Next cache collection; the background worker runs every 30 seconds |
| `validation` | Next validation operation; an operation already running retains its configuration |
| `allowed_origins` | Next HTTP request, including existing MCP sessions |
| `data_dir`, `listen`, `mount_roots`, `directory_roots` | After a daemon restart |
| `credentials`, token-file contents, `administrators`, `docker`, process environment | After a daemon restart |

Origins use the exact `scheme://host[:port]` form for HTTP or HTTPS, without a path, trailing slash, credentials, wildcard, or an explicit default port. Same-origin dashboard requests and requests without Origin remain supported. Bearer authentication still applies. This setting controls Origin checks and does not enable CORS response headers.

External file edits are checked once per second. Each candidate is validated before publication. Invalid JSON, invalid limits, unavailable files, and changes to any startup field retain the complete active configuration and appear as an error on the settings page. When changing a startup field, restart the daemon to apply the whole file. Correcting the file clears the error on the next check. Prefer replacing the file atomically to avoid submitting partially written JSON. The daemon does not watch token-file contents.

The authenticated HTTP interface provides:

- `GET /api/settings`: active `runtime`, read-only `startup` fields, `config_path`, a `revision`, and `reload_error`.
- `PUT /api/settings`: `{ "revision": "<revision from GET>", "runtime": { "cache": { ... }, "validation": { ... }, "allowed_origins": [], "remote": null } }`. Supply the complete runtime object returned by GET. A successful response returns the saved settings.

Both endpoints require membership in `administrators`; workspace grants do not grant settings access. Responses contain token-file paths but never token contents or S3 environment credentials. Missing authentication returns 401, insufficient settings access returns 403, and a stale revision or a detected external file edit returns 409. Validation failure preserves the active settings and file. The UI preserves unsaved form values when it detects a newer revision and offers an explicit reload action.

## Switching storage while running

Select local or S3 mode in Settings and save. `remote.enabled: false` retains S3 settings and credentials for later use. Direct configuration-file edits use the same transition.

Enabling S3 checks read, conditional-write and delete access, copies locally known history and branch references, then persists configuration and replaces the connection. Returning to local storage downloads evicted history and disables historical cache eviction. Mounts and working directories remain available; pending publications are retained until S3 is enabled again.

Some operations wait while history is transferred. Missing content, insufficient local space, an advanced destination branch, credential errors or a configuration write failure retain the active connection. The destination may already contain immutable objects and references; retrying continues safely. Complete pending ownership transfers before changing storage. Failed external file edits retain the active runtime settings, display an error, and are retried by the file watcher. The connection check creates and deletes temporary objects beneath `.connection-check/` in the configured namespace.

## S3 fields

The following object belongs under the **top-level `remote` key** in `agentfs.json`:

```json
{
  "remote": {
    "bucket": "agentfs",
    "prefix": "agentfs/v1",
    "region": "us-east-1",
    "endpoint": "http://127.0.0.1:9000",
    "allow_http": true
  }
}
```

This is a configuration fragment for a local RustFS S3 API endpoint. Keep the other generated fields. `endpoint` is the S3 API base URL, without the bucket name, credentials, query parameters, or a fragment. The adapter uses path-style requests and appends the bucket name. Use the S3 API port supplied by your deployment.

| `remote` field | Required / default | Behavior |
| --- | --- | --- |
| `enabled` | `true` | Use `false` for local mode while retaining S3 settings. |
| `credentials` | Omitted or `null` | Use the service environment or IAM role, or supply `access_key_id`, `secret_access_key`, and optional `session_token`. |
| `bucket` | Required, nonempty string | Existing bucket name. The daemon does not create buckets. |
| `prefix` | `agentfs/v1` | Shared namespace within the bucket; leading/trailing `/` are removed and the result must be a valid, nonempty object path. |
| `region` | `us-east-1` | Signing region; use the bucket's region for AWS S3. |
| `endpoint` | Omitted or `null` | Optional custom S3 base URL. With no endpoint override, the dependency derives the AWS regional endpoint. |
| `allow_http` | `false` | Set `true` for an explicitly selected HTTP development endpoint. HTTPS endpoints keep `false`. |

For AWS S3, replace the bucket and region, and omit a custom endpoint:

```json
{
  "remote": {
    "bucket": "your-existing-bucket",
    "prefix": "agentfs/v1",
    "region": "eu-west-1",
    "allow_http": false
  }
}
```

## Credentials and environment precedence

The dashboard can persist S3 credentials in the configuration file, using Unix mode `0600`. GET settings blanks `secret_access_key` and `session_token`; a PUT with the same Access Key ID and a blank secret retains the existing credentials. Supply the secret again when changing a session token. Set `credentials` to `null` to use environment credentials. Configured credentials override environment credentials.

The **daemon process** reads S3 credentials. Set these variables in the shell or service environment that starts `agentfsd serve`. `AGENTFS_TOKEN_FILE` authenticates the CLI to OpenCSG Workspace separately.

```sh
export AWS_ACCESS_KEY_ID='your-access-key-id'
export AWS_SECRET_ACCESS_KEY='your-secret-access-key'
# Temporary credentials also require AWS_SESSION_TOKEN.
./target/debug/agentfsd serve --config "$AGENTFS_DIRECTORY/agentfs.json"
```

PowerShell uses `$env:AWS_ACCESS_KEY_ID = 'your-access-key-id'` and `$env:AWS_SECRET_ACCESS_KEY = 'your-secret-access-key'` before starting `agentfsd.exe`.

The locked `object_store` dependency supports the following credential sources, in this order:

1. `AWS_ACCESS_KEY_ID` and `AWS_SECRET_ACCESS_KEY`, with optional `AWS_SESSION_TOKEN`. Supply both key fields together.
2. `AWS_WEB_IDENTITY_TOKEN_FILE` and `AWS_ROLE_ARN`, with optional `AWS_ROLE_SESSION_NAME`.
3. `AWS_CONTAINER_CREDENTIALS_RELATIVE_URI` for a container task role.
4. `AWS_CONTAINER_CREDENTIALS_FULL_URI` together with `AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE` for EKS Pod Identity.
5. Instance metadata credentials when the preceding sources are absent.

The current adapter does not load `~/.aws/credentials`, `AWS_PROFILE`, or AWS CLI SSO sessions. Export resolved credentials or use one of the supported role providers. Static environment credentials are loaded at startup; restart the daemon after replacing them.

OpenCSG Workspace calls `AmazonS3Builder::from_env()` and then applies its configuration. The configured `bucket`, `region`, and `allow_http` values, including their defaults, override the corresponding AWS environment settings. A configured `endpoint` overrides both `AWS_ENDPOINT` and `AWS_ENDPOINT_URL_S3`. When `endpoint` is omitted, environment settings remain effective; remove both environment variables to use the default AWS regional endpoint. Path-style requests and ETag conditional writes are selected by the adapter.

## Storage requirements and verification

The storage service must support GET/HEAD, listing by prefix, object writes, multipart upload and abort, and strongly consistent reads. OpenCSG Workspace creates immutable objects with `If-None-Match: *`, completes multipart uploads with the same condition, and updates references using `If-Match` with the previous ETag. Conditional write behavior is described in the [AWS S3 documentation](https://docs.aws.amazon.com/AmazonS3/latest/userguide/conditional-writes.html). Files larger than 8 MiB use multipart upload; part sizes range from 8 to 64 MiB according to file size. Restrict storage credentials to the selected bucket and namespace.

Daemon startup constructs the S3 client; it does not prove that the bucket or credentials work. After importing content using the [CLI example](cli.md), explicitly push and inspect the original import receipt:

```sh
jq -n --arg workspace "$WORKSPACE" '{workspace:$workspace,direction:"push"}' |
  agentfs call sync --request-id "$RUN_ID-push" --json -
jq -n --arg operation "$(jq -r '.id' "$AGENTFS_DIRECTORY/demo-import.json")" '{operation:$operation}' |
  agentfs call operation_get --json -
```

Check `remote_confirmed` on the original operation. `workspace_status.capabilities.remote_configured` reports configuration presence, and `pending_jobs` reports the queue length. The daemon retries pending synchronization every three seconds. A `local_saved: true` receipt can still contain a remote error; retain its operation ID and follow [recovery guidance](operations.md).

For a compatibility check covering conditional writes, competing updates, 20 MiB multipart content, and two-daemon replication, use the isolated [S3/RustFS tests](validation.md). Those tests use a separate test bucket and are not a production health check.

## Multiple machines

Configure each daemon with the same S3 endpoint, bucket, prefix, and signing region. Each machine needs its own initialized state directory and `LocationId`; share the workspace ID returned by the source daemon. The destination's authenticated principal must be the workspace owner or have suitable grants. Tokens can differ between machines.

Push on the source, then call `sync` with `direction: "pull"` and the same workspace ID on the destination. Pull imports history and remote branch references. Use a retained revision to create a local branch, or complete an online `ownership_transfer` before adopting an existing writable branch. See [operations](operations.md) and the [MCP reference](mcp.md).

Configuration definitions: [schema](../crates/agentfs-config/src/file.rs), [runtime store](../crates/agentfs-config/src/store.rs), and [S3 adapter](../crates/agentfs-s3/src/lib.rs). Dependency versions are recorded in [Cargo.lock](../Cargo.lock).

# AgentDrive

[English](README.md) | [简体中文](README.zh-CN.md)

**Share the files. Pick up the work.**

AgentDrive gives people and agents a persistent project space. Agents read source material, write documents and edit code in a local directory. Their work can be saved remotely so an agent on another machine can continue. Branches keep parallel tasks separate, and versions record how the results evolve.

Keep using your preferred agent, editor and terminal. MCP lets agents manage saving, synchronization, branches and handoffs. People can open the **Archive** to read the latest files in each project branch.

> Development preview; the first formal release has not shipped. AgentDrive is the current product name.

![Archive: browse a branch and read its handoff document](docs/images/archive.png)

*Demo project: market research. Read the agent handoff in `/reports/交接` alongside local save state and remote confirmation. The current dashboard is in Chinese.*

## Keep the project moving

| When you need to… | AgentDrive helps you… |
| --- | --- |
| Work locally and save remotely | Use ordinary file tools, keep local work, and synchronize results through configured S3 / RustFS or OpenCSG Codes storage. |
| Hand work to an agent elsewhere | Save the brief, input files, results and next steps together; pull them on the receiving machine. |
| Try several approaches in parallel | Branch from shared material, complete separate tasks, review changes and merge the results. |
| Check progress yourself | Browse current files in the Archive, then inspect their origins and changes in Branches & Versions. |

## How to use it

### 1. Connect a local project and start working

Follow [installation and startup](docs/installation.md), then connect AgentDrive to an MCP-capable agent client. An existing local directory can be imported and mounted back at the same path. Pause writes before converting it, and restart the agent after conversion.

Ask your agent:

> Connect this project to AgentDrive. Keep its existing files, create a branch for this task, and tell me the working directory and branch name.

The agent creates, mounts or adopts a workspace through MCP, then continues using its normal file tools. Changes are automatically saved. Ask it to save a named version when you reach a meaningful milestone.

### 2. Synchronize local results to remote storage

Open **Settings** in the dashboard and choose a remote backend. Configure S3 / RustFS with an endpoint, bucket and credentials, or OpenCSG Codes with a domain, repository and access token. Machines collaborating on a project use the same storage destination and authorized project identity, with independent local configuration and state directories. [Connection guide](docs/configuration.md)

> Save this branch remotely. After remote publication is confirmed, report the workspace ID, branch, version and handoff document path.

The agent can request remote durability with `mount_save` and inspect `remote_confirmed` in the operation receipt. If waiting times out, keep polling the same operation. **Local saving and remote confirmation are separate states.** The Archive reports the latest known remote confirmation.

### 3. Let an agent on another machine continue

Keep a `HANDOFF.md` in the project with the goal, completed work, important files, validation steps and remaining tasks. Save it remotely with the results.

Give the receiving agent the workspace ID, source branch or revision and handoff path:

> Pull the latest confirmed remote project data, read HANDOFF.md, create my branch from the specified source, mount it locally and continue the next task.

The receiving agent uses `sync`, creates a branch from the intended source and mounts it. To continue writing **the same branch**, stop writes and release mounts on the previous machine, transfer write ownership with `ownership_transfer`, then pull and confirm adoption at the destination location. [Handoff and recovery](docs/operations.md)

Handoffs carry files, versions and branch information. Task context can travel as project documents. Running processes, chat history and client credentials are managed by each agent client.

### 4. Read results in the Archive and inspect changes in history

Open the dashboard, select a project and enter **Archive**:

- Select a branch to browse its current directory; a file preview opens automatically.
- Read Markdown as a document or source. Page through text and inspect binary bytes in hexadecimal.
- Click any segment of the `/a/b/c` directory path to navigate. Filter the current directory, reveal hidden files and return to the same project, branch and directory through its URL.
- See local progress, remote confirmation and directory usage separately. Browsing leaves the agent's mounted branch unchanged.
- Files reflect locally known current contents, including unsaved or uncommitted edits. Ask the agent to pull remote updates before reviewing new work from another machine.

![Branches and versions: inspect origins and revision history](docs/images/branches.png)

*The same demo project in Branches & Versions. Double-click a revision to inspect its metadata, changed files and text differences.*

## Built for agents to operate

For a client that supports stdio MCP, add this configuration and replace the example paths with local absolute paths:

```json
{
  "mcpServers": {
    "agentdrive": {
      "command": "/absolute/path/agentdrive/target/debug/agentdrive",
      "args": ["mcp"],
      "env": {
        "AGENTDRIVE_TOKEN_FILE": "/absolute/path/to/agentdrive/token"
      }
    }
  }
}
```

Keep the AgentDrive service running. Agents can discover tool schemas and manage workspaces, branches, saves, synchronization and handoffs. Streamable HTTP clients can connect to `http://127.0.0.1:7421/mcp` with the local bearer token. CLI commands support terminal workflows and scripts; ACP / Codex app-server integrations connect working directories with end-of-task saving. [Agent integrations](docs/integrations.md) · [MCP tools](docs/mcp.md) · [CLI](docs/cli.md)

A single `branch_switch` call completes a branch change. If a process occupies the directory, follow the response instructions to close it or restart the agent, then retry the same operation.

## Get started

Build from source on Linux, macOS or Windows. Native mounts require FUSE, FUSE-T / macFUSE or WinFsp respectively. [Installation, dependencies and startup commands](docs/installation.md)

- [Remote storage configuration](docs/configuration.md): local storage, S3, RustFS, OpenCSG Codes and multiple machines.
- [Operations and recovery](docs/operations.md): publication confirmation, handoffs, merging and recovery.
- [Documentation index](docs/README.md): complete usage references and developer documentation.

AgentDrive is licensed under Apache-2.0. The included WinFsp Rust wrapper retains its MIT license; mount components have their own license terms.

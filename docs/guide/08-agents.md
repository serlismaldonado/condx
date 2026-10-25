# AI agents (MCP)

## They can

- Look at everything: status, logs, CI, app telemetry (`get_overview`, `get_report`, `get_function_logs`…).
- Request jobs (`request_job`) and follow them (`get_job`, `get_job_log`).
- Check a promotion's conditions (`check_promotion`).
- Start workflows (`run_workflow`) and follow their runs.
- Cancel the jobs they requested themselves.

## They can't

- **Approve.** Whatever they request that needs approval waits for you in this app, with a notice.
- Change the rules in Settings or read and save deploy keys.

## Example

An agent requests "Rehearsal" and "Deploy to cloud dev": the rehearsal runs on its own; the deploy shows up in **Approvals** until you approve it.

## Connect an agent

**Settings › Connect an agent** has the commands ready to copy (Claude Code, Cursor, Codex and
HTTP) and when the MCP was last used. In Claude Code:

```
claude mcp add condx -- ~/.condx/daemon/bin/condxd mcp
```

Any stdio MCP client uses `condxd mcp`; it needs the daemon running (`condxd install`). There are no
accounts: the bridge uses the local token `~/.condx/daemon/token`, which can't approve.

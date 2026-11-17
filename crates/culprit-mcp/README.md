# culprit-mcp

## Purpose

Let coding agents inspect Culprit through the Model Context Protocol over standard input and output.

## Responsibilities

- Expose tools for connections, findings, queries, plans and schema.
- Apply connection-level agent permissions.
- Require explicit approval for the opt-in read-only query tool.
- Record tool calls in the audit log.
- Optionally analyze verified local collector captures without opening a database.
- Configure supported coding-agent clients.

## How it fits

The `culprit mcp` command hosts this server. Tools use the engine's `Workspace` API and the same analysis as the app.
`--evidence-outbox DIRECTORY` adds the local-capture capability. An embedding host can inject its reader directly;
the default server does not depend on the collector or warehouse adapters.

## Main entry points

- [Server](src/lib.rs): tool handling and audit ownership.
- [Tools](src/tools.rs): tool definitions and dispatch.
- [Client setup](src/init.rs): coding-agent configuration.

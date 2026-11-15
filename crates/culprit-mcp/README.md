# culprit-mcp

## Purpose

Let coding agents inspect Culprit through the Model Context Protocol over standard input and output.

## Responsibilities

- Expose tools for connections, findings, queries, plans and schema.
- Apply connection-level agent permissions.
- Require explicit approval for the opt-in read-only query tool.
- Record tool calls in the audit log.
- Configure supported coding-agent clients.

## How it fits

The `culprit mcp` command hosts this server. Tools use the engine's `Workspace` API and the same analysis as the app.

## Main entry points

- [Server](src/lib.rs): tool handling and audit ownership.
- [Tools](src/tools.rs): tool definitions and dispatch.
- [Client setup](src/init.rs): coding-agent configuration.

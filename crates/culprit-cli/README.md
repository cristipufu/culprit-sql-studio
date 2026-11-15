# culprit-cli

## Purpose

Expose Culprit through the `culprit` terminal command.

## Responsibilities

- Parse commands and connection options.
- Present reports, findings, plans and live activity as text or JSON.
- Expose collection, delivery and fleet administration commands.
- Start the MCP server and configure coding-agent integrations.

## How it fits

Commands call the engine's `Workspace` services. Database collection and diagnostic decisions belong to the shared backend.

## Main entry points

- [Commands and arguments](src/main.rs): command tree and dispatch.
- [Command handlers](src/commands.rs): common user operations.
- [Output](src/output/mod.rs): terminal and machine-readable presentation.
- [Collector service](src/service_cmd.rs): unattended runtime commands.

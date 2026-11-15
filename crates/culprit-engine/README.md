# culprit-engine

## Purpose

Provide the application services used by the CLI, desktop app and MCP server.

## Responsibilities

- Manage saved connections, credential references and reports.
- Select providers and coordinate bounded database operations.
- Run the analysis pipeline and expose its results.
- Enforce execution permissions, confirmations and agent approvals.
- Compose collector, recovery and telemetry adapters when their features are enabled.

## How it fits

`Workspace` connects the product's interfaces to database providers and shared analysis. Native SQL and destination protocols stay in their adapters.

## Main entry points

- [Workspace](src/lib.rs): public application API.
- [Connections](src/connection.rs): routing and credential resolution.
- [Scan](src/scan.rs): collection and report creation.
- [Feature configuration](Cargo.toml): optional providers and collector integrations.

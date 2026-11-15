# culprit-auth

## Purpose

Define how adapters receive credentials and how the host resolves authentication.

## Responsibilities

- Define bearer-token, database-token and access-key interfaces.
- Resolve independent named credential profiles.
- Define secret-store and bounded command-runner contracts.
- Represent sign-in prompts and events.

## How it fits

The engine supplies host services. Authentication providers implement native login methods; database, recovery and telemetry adapters receive credentials through these interfaces.

## Main entry points

- [Credential and host contracts](src/lib.rs): provider interfaces and authentication services.
- [Manifest](Cargo.toml): dependencies of the shared authentication boundary.

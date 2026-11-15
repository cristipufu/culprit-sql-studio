# culprit-auth-azure

## Purpose

Implement Azure and Microsoft Entra authentication behind the shared credential interfaces.

## Responsibilities

- Acquire tokens for explicitly configured managed or workload identities.
- Support interactive Azure sign-in through the CLI adapter.
- Validate token audiences and identity endpoints.
- Cache tokens within their owning identity and refresh lifetime.

## How it fits

The host selects the identity. Database, storage and ADX adapters receive bearer tokens through `culprit-auth`; each can use a different profile.

## Main entry points

- [Unattended identity](src/lib.rs): configuration, audiences and token ownership.
- [CLI authentication](src/cli.rs): interactive Azure authentication.
- [Endpoint policy](src/endpoint.rs): native identity endpoints.

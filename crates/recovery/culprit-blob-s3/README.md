# culprit-blob-s3

## Purpose

Store and retrieve collector recovery objects through the S3 API.

## Responsibilities

- Implement bounded object reads and uploads.
- Enforce conditional writes using object versions.
- Sign requests with injected access-key credentials.
- Validate endpoints, regions and object paths.

## How it fits

This adapter implements the collector's recovery contracts independently of the analytical destination. Authentication is supplied through `culprit-auth`.

## Main entry points

- [Transport](src/lib.rs): object operations and recovery-provider integration.
- [Configuration](src/config.rs): endpoint and bucket settings.
- [Signing](src/signing.rs): native request signatures.

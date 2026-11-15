# culprit-blob-azure

## Purpose

Store and retrieve collector recovery objects in Azure Blob Storage.

## Responsibilities

- Implement bounded object reads and uploads.
- Enforce conditional writes using object versions.
- Validate storage endpoints and object paths.
- Support Azure Blob staging where the delivery configuration requires it.

## How it fits

The collector defines recovery semantics. This adapter handles the Azure protocol using an injected bearer-token provider; the ADX adapter separately handles analytical ingestion.

## Main entry points

- [Transport](src/lib.rs): object operations and provider integration.
- [Configuration](src/config.rs): endpoints and container settings.
- [HTTP](src/http.rs): Azure Blob request handling.

# culprit-store-adx

## Purpose

Deliver captured telemetry to Azure Data Explorer and read analytical evidence back.

## Responsibilities

- Generate Kusto table, mapping and function definitions.
- Submit prepared telemetry through supported ingestion modes.
- Verify landed rows and publication records.
- Query retained evidence through typed, bounded reads.

## How it fits

The collector owns capture and delivery state; this adapter owns Kusto protocols and queries. Credentials and recovery storage are supplied independently.

## Main entry points

- [Sink](src/sink.rs): the collector's destination interface.
- [Deployment](src/deployment.rs): Kusto schema and function definitions.
- [Ingestion](src/ingestion.rs): ingestion configuration and dispatch.
- [Evidence](src/evidence.rs): typed analytical readback.

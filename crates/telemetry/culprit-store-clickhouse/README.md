# culprit-store-clickhouse

## Purpose

Deliver captured telemetry to ClickHouse and read analytical evidence back.

## Responsibilities

- Generate ClickHouse table definitions.
- Insert the collector's prepared analytical rows.
- Verify landed rows and publication records.
- Query retained evidence through typed, bounded reads.

## How it fits

The collector owns capture and delivery state; this adapter owns ClickHouse HTTP and SQL. This is a telemetry destination, separate from a database-analysis provider.

## Main entry points

- [Sink](src/sink.rs): the collector's destination interface.
- [Deployment](src/deployment.rs): ClickHouse schema definitions.
- [Client](src/client.rs): native HTTP requests.
- [Evidence](src/evidence.rs): typed analytical readback.

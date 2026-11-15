# culprit-read

## Purpose

Share the mechanics of bounded database reads across providers.

## Responsibilities

- Deliver rows with byte and row limits and backpressure.
- Keep incomplete native results provisional.
- Track cancellation, native completion and fenced reads.
- Coordinate source admission and durable ownership state.
- Support bounded snapshots and frozen observations.

## How it fits

Database drivers use these primitives while retaining their own connections, SQL and protocol handling. Scans and unattended collection share the same mechanics.

## Main entry points

- [Row delivery](src/lib.rs): buffered and streaming output.
- [Read fence](src/fence.rs): native work and shutdown state.
- [Admission](src/admission.rs): shared source ownership.
- [Snapshots](src/snapshot.rs): bounded snapshot readers.

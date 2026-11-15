# culprit-collector

## Purpose

Capture database telemetry unattended and deliver it durably to an analytical store.

## Responsibilities

- Schedule fleet collection and enforce capture budgets.
- Persist captured ranges and pending delivery state.
- Coordinate recovery objects, delivery receipts and publication.
- Prepare shared analytical rows for destination adapters.
- Retain incident and investigation lifecycle state.

## How it fits

Authenticated database providers supply evidence. Recovery adapters preserve it; telemetry stores ingest it. The collector coordinates those contracts independently of their native protocols.

## Main entry points

- [Capture](src/capture.rs): bounded evidence acquisition.
- [Fleet](src/fleet/mod.rs): scheduling and source ownership.
- [Delivery](src/delivery/mod.rs): recovery, verification and publication.
- [Analytical projection](src/analytical/mod.rs): shared warehouse preparation.

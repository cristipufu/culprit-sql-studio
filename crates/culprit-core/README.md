# culprit-core

## Purpose

Define the database facts and contracts shared by the rest of Culprit.

## Responsibilities

- Model snapshots, queries, plans, findings and telemetry evidence.
- Define provider and check interfaces.
- Describe engine capabilities, units and terminology.
- Supply shared formatting, localization and pure evidence helpers.
- Generate the TypeScript types used by the app.

## How it fits

Providers produce these types; checks analyze them; the engine and user interfaces consume the results.

## Main entry points

- [Model](src/model/mod.rs): snapshots, sessions and workload types.
- [Provider contract](src/provider.rs): operations a database adapter can offer.
- [Engine facts](src/engines/mod.rs): capabilities and native semantics.
- [Telemetry contracts](src/telemetry/mod.rs): identities, observations and delivery evidence.

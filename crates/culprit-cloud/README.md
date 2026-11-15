# culprit-cloud

## Purpose

Discover cloud-hosted databases and read their cloud monitoring metrics.

## Responsibilities

- Provide Azure and AWS discovery adapters.
- Read Azure Monitor and CloudWatch metrics.
- Bound discovery pages and metric requests.
- Accept explicit authentication and transport interfaces from the host.

## How it fits

The engine composes these optional cloud services. Database providers collect native evidence; telemetry-store crates handle ADX and ClickHouse delivery.

## Main entry points

- [Service interfaces](src/lib.rs): discovery, metrics and injected transports.
- [Discovery](src/discovery.rs): database discovery contracts and adapters.
- [Metrics](src/metrics/mod.rs): cloud metric reads and caching.
- [Providers](src/providers.rs): Azure and AWS service composition.

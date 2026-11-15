# culprit-checks

## Purpose

Turn captured database evidence into diagnoses and developer advice.

## Responsibilities

- Run pure checks against a snapshot.
- Separate evidence decisions from localized wording.
- Merge related symptoms and rank findings.
- Build dashboard, digest, comparison and investigation views.

## How it fits

The engine supplies captured facts. This crate analyzes them without opening database connections. External checks can join the same pipeline.

## Main entry points

- [Public API](src/lib.rs): analysis, registered checks and report views.
- [Pipeline](src/pipeline.rs): prepare → detect → refine → rank.
- [Checks](src/checks/mod.rs): diagnostic areas and registration.
- [Views](src/views/mod.rs): explanations built from reports.

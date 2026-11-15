# culprit-app

## Purpose

Host the React interface as a native desktop application with Tauri.

## Responsibilities

- Expose backend commands to the frontend.
- Own the desktop `Workspace` and native application lifecycle.
- Coordinate window-scoped requests and sign-in events.
- Integrate updates and supported native screenshot behavior.

## How it fits

The interface lives in `app/src`. This Rust package bridges it to `culprit-engine` and operating-system services; shared collection and analysis stay in the backend crates.

## Main entry points

- [Desktop bridge](src/lib.rs): commands, application state and startup.
- [Request ownership](src/requests.rs): window-scoped request lifecycles.
- [Configuration](tauri.conf.json): application and bundle settings.

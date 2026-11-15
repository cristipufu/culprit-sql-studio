# culprit-auth-aws

## Purpose

Resolve AWS credentials and IAM database authentication through an explicitly selected AWS CLI profile.

## Responsibilities

- Run AWS authentication commands through the host's command interface.
- Generate IAM database login tokens.
- Supply access keys for signed requests, including temporary session credentials.
- Provide the bundled RDS certificate authorities and actionable sign-in errors.

## How it fits

The engine supplies the command runner. Database providers receive login credentials; recovery adapters receive access keys through `culprit-auth`.

## Main entry points

- [AWS adapter](src/lib.rs): profile selection, database authentication and CLI errors.
- [Access keys](src/access_keys.rs): temporary credential acquisition and caching.

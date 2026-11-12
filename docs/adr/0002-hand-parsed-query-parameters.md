# 2. Parse query parameters by hand

Status: accepted, 2026-10

## Context

Monarch's routes are FastAPI. They accept repeated keys (`category=a&category=b`) and answer
invalid parameters with 422 and pydantic's messages. axum's typed `Query` extractor can't read
repeated keys, and its rejections have a different shape.

## Decision

`http.rs` parses the raw query string itself (`Params`) and turns every failure into `ApiError`,
which renders FastAPI's 422 / 404 bodies.

## Consequences

- Clients written against Monarch see the same errors from this service.
- New parameters are read through `Params` rather than declared on a struct.

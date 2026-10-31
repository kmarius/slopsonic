# Agents Guidelines

## Project Overview

Rufin is a native Linux desktop client for [Navidrome](https://www.navidrome.org/), a self-hosted music streaming server. The codebase is a Rust workspace with a GTK frontend.

## Scope

The project should focus exclusively on being a polished, native Linux music player that connects to Navidrome servers.

## Non-Features

Do not implement or expand:

- **Other server backends** — Navidrome only, no Subsonic/Jellyfin/Plex/etc.
- **Metadata editing** — Read-only client; editing belongs in the server or dedicated tools
- **File management** — No local library scanning, file organization, or downloads
- **Rufin Connect / sync** — No cross-device sync or remote control features
- **Headless/controller mode** — This is a desktop GUI application
- **Windows/macOS/Android builds** — Linux only; ignore existing cross-platform code

## Architecture

- `crates/` — Rust workspace with modular crates
- `web/` — Legacy/auxiliary; not the focus
- `android/` — Out of scope

When in doubt, prefer simplicity and removing code over adding features.

# Agents Guidelines

## Project Overview

Rufin is a native Linux desktop client for [Navidrome](https://www.navidrome.org/), a self-hosted music streaming server. The codebase is a Rust workspace with a GTK frontend.

## Scope

The project should focus exclusively on being a polished, native Linux music player that connects to Navidrome servers.

## Non-Features

Do not implement or expand:

- **Other server backends** — Navidrome/Subsonic only, no Jellyfin/Plex/etc.
- **Metadata editing** — Read-only client; editing belongs in the server or dedicated tools
- **File management** — No local library scanning, file organization, or downloads
- **Rufin Connect / sync / casting** — No cross-device sync, remote control, or UPnP/Chromecast casting
- **Headless/controller mode** — This is a desktop GUI application
- **Windows/macOS/Android builds** — Linux only; ignore existing cross-platform code
- **Lyrics, visualizer, waveform seekbar**
- **Discord rich presence, update checks/self-updater**
- **Listening overview, playback speed/pitch, audio output selection** — the system default output is always used
- **Private mode, localization**

## Architecture

- `crates/` — Rust workspace with modular crates
- `android/` — Out of scope
- Dead code is a build error (`-D warnings`); remove unused code and dependencies rather than leaving them

When in doubt, prefer simplicity and removing code over adding features.

## Commits

Keep commit messages extremely brief—a few words, not sentences.

## Code Style

Do not add comments to code.

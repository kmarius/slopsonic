# Agents Guidelines

## Project Overview

Slopsonic is a native Linux desktop client for [Navidrome](https://www.navidrome.org/), a self-hosted music streaming server. The codebase is a Rust workspace with a GTK frontend.

## Scope

The project should focus exclusively on being a polished, native Linux music player that connects to a Navidrome or OpenSubsonic server.

## Features

Planned or supported, and not to be removed:

- **Downloads and offline play**
- **Transcoding** — for playback and for downloads
- **MPRIS control** — desktop media controls and remote control via MPRIS

More to be added later.

## Non-Features

Do not implement or expand:

- **Other server backends** — Navidrome/Subsonic only, no Jellyfin/Plex/etc.
- **Metadata editing** — Read-only client; editing belongs in the server or dedicated tools
- **File management** — No local library scanning or file organization
- **Playing from disk** — No mapping server paths to local music folders; playback streams from the server or uses downloads
- **Rufin Connect / sync / casting** — No cross-device sync, remote control, or UPnP/Chromecast casting
- **Headless/controller mode** — This is a desktop GUI application
- **Windows/macOS/Android builds** — Linux only. Android, macOS and Windows support was removed because it can not be tested; some cross-platform and Android-specific code may remain and should be removed
- **Backup and restore**
- **Audio fingerprinting and external metadata lookup** — no AcoustID/MusicBrainz or other third-party lookups
- **Lyrics, visualizer, waveform seekbar**
- **Discord rich presence, update checks/self-updater**
- **Listening overview, playback speed/pitch, audio output selection** — the system default output is always used
- **Private mode**
- **Activity and queue import/export** — no CSV/JSONL export or import of listening history, queues or ratings
- **Partial star ratings** — whole stars only
- **Local loudness analysis** — no EBU R128 scanning; only server and embedded ReplayGain are applied
- **Mono downmix**
- **Tray icon, start minimized and keep running after closing the window** — closing the window quits the app
- **Desktop notifications** — no now-playing notifications
- **System keyring** — credentials are stored in the config file only
- **Custom themes** — only light/dark/system color scheme plus an accent color
- **Smart playlists and the rule editor**
- **Playlist folders**
- **Localization**

## Architecture

- `crates/` — Rust workspace with modular crates
- Single server: the app talks to exactly one Navidrome or OpenSubsonic server. Both share the one Subsonic client in `crates/sources/src/subsonic/`; Navidrome-only features are gated on the flavor. Do not add provider abstractions, multi-server selection or per-source UI
- Settings keep a list of configured servers on disk, but the app enforces at most one
- The library schema is versioned in `crates/library/src/schema.rs` (`SCHEMA_VERSION`); add migrations in `initialize_durable`
- Dead code is a build error (`-D warnings`); remove unused code and dependencies rather than leaving them

When in doubt, prefer simplicity and removing code over adding features.

## Commits

Keep commit messages extremely brief—a few words, not sentences.

## Code Style

Do not add comments to code.

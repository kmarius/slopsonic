# Agents Guidelines

## Project Overview

Slopsonic is a native Linux desktop client for [Navidrome](https://www.navidrome.org/), a self-hosted music streaming server. The codebase is a Rust workspace with a GTK frontend.

## Scope

The project should focus exclusively on being a polished, native Linux music player that connects to a Navidrome or OpenSubsonic server.

## Features

Planned or supported, and not to be removed:

- **Navidrome and OpenSubsonic compliance** — stay compatible with both servers' APIs
- **Downloads and offline play**
- **Transcoding** — for playback and for downloads
- **MPRIS control** — desktop media controls and remote control via MPRIS

More to be added later.

## Non-Features

Do not implement or expand. If you find leftover code for a non-feature, remove it:

- **Other server backends** — Navidrome/Subsonic only, no Jellyfin/Plex/etc.
- **Metadata editing** — Read-only client; editing belongs in the server or dedicated tools
- **File management** — No local library scanning or file organization
- **Playing from disk** — No mapping server paths to local music folders; playback streams from the server or uses downloads
- **Rufin Connect / sync / casting** — No cross-device sync, remote control, or UPnP/Chromecast casting
- **Headless/controller mode** — This is a desktop GUI application
- **Windows/macOS/Android builds** — Linux only. Android, macOS and Windows support was removed because it can not be tested
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
- **App-owned playlists** — playlists live on the server; no local-only playlists
- **Local listening history** — play counts and last-played come from the server; the app only queues undelivered scrobbles
- **Local-only item state** — favorites and ratings live on the server; a local value exists only while its edit waits to be delivered
- **Playlist folders**
- **Localization**

## Architecture

- `crates/` — Rust workspace with modular crates:
  - `slopsonic` — thin binary: allocator, diagnostics, tokio runtime, starts the GTK app
  - `slopsonic-core` — composition root and product operations (playback actor, source lifecycle, settings, routes)
  - `sources` — the Subsonic/Navidrome client; the only crate that talks to the server
  - `library` — SQLite persistence: catalog cache, queue, download records, outboxes for scrobbles, favorites and ratings
  - `playback` — backend-agnostic playback trait, settings and view types
  - `playback-gstreamer` — GStreamer implementation of the playback backend
  - `audio-processing` — transcoding, EQ/crossfeed/balance effects, spectrum FFT
  - `downloads` — offline download queue and on-disk storage
  - `artwork` — artwork selection, fetch, cache and decode
  - `scrobbling` — Last.fm, Libre.fm and ListenBrainz delivery
  - `secrets` — credential store backed by the config file
  - `app-identity` — app id and display name, release vs. devel
  - `process-memory` — glibc allocator tuning and memory reclaim
  - `desktop-integration` — MPRIS media controls
  - `gtk-shell` — window, navigation and native actions; composes the other `gtk-*` crates
  - `gtk-library` — catalog pages: albums, artists, playlists, search, home
  - `gtk-player` — bottom bar, queue, fullscreen player, equalizer UI
  - `gtk-preferences` — preferences, server setup, appearance
  - `gtk-media-menus` — context menus, playlist picker, media details
  - `gtk-editors` — playlist artwork editing
  - `gtk-widgets` — shared GTK primitives
- Single server: the app talks to exactly one Navidrome or OpenSubsonic server. Both share the one Subsonic client in `crates/sources/src/subsonic/`; Navidrome-only features are gated on the flavor. Do not add provider abstractions, multi-server selection or per-source UI
- Settings store the single configured server
- The library schema is versioned in `crates/library/src/schema.rs`: `BASE_VERSION` plus one entry in `MIGRATIONS` per change (`SCHEMA_VERSION` is derived). Schema changes during development add a migration step. Before the first release, migrations may be squashed into `STORE_SCHEMA` and `BASE_VERSION` raised; after release they are permanent and never removed
- Local state is limited to settings, the saved queue, downloads and outboxes of undelivered server writes (scrobbles, favorites, ratings). Everything else in the database is a cache of the server that can be rebuilt by syncing
- Dead code is a build error (`-D warnings`); remove unused code and dependencies rather than leaving them

When in doubt, prefer simplicity and removing code over adding features.

## Build

- `cargo build` / `cargo run -p slopsonic`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo fmt --all`
- `cargo test --workspace`

## Commits

Keep the commit subject to one short line that says what changed, as `area: phrase` (e.g. "artists: show downloaded badge on title column only"), not a full sentence. The body may add up to two lines of text.

## Code Style

Do not add comments to code. Short doc comments (`///`, `//!`) are allowed.

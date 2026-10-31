# Rufin

Rufin is a powerful, fast and easy to use native music player for Navidrome.

## 🎵 Playback

- True gapless playback and configurable equal-power crossfade mode
- ReplayGain/EBU R128 analysis and true-peak measurement
- 10-band equalizer with presets, perceptual/linear volume scale preference and fade on play/pause
- Auto DJ that keeps the queue filled; server recommendations when available with a smart local fallback
- Random play with filters for track count, year range, genre and played/unplayed status
- Fullscreen player
- Broad codec support [^codecs]
- Audio output device selection
- Configurable playback speed and pitch

## 📚 Library

- Drag songs from your queue or lists to playlists, or select multiple of them with your keyboard batch operations
- Combine music from several servers in a single listening session with global playlists
- Download tracks manually or through automatic rules, then keep using the same remote library while offline
- Path matching between music servers and local folders, allowing local playback while keeping server activity and history
- Automatic metadata and artwork caching; and in-app artwork search functionality
- Rating support, with an option to enable visual-only partial stars for OpenSubsonic
- Virtualized, smooth to scroll pages
- Granular import/export backup options including credentials, automatic backups with password protection are available
- Listening activity can be exported as Last.fm/ListenBrainz compatible `.csv` files

## 🌍️ Discovery

- Server-provided artist, track, album, playlist and genre radios, including recommendations from server plugins
- App owned smart (i.e. dynamic) playlists with custom sorting and limits; can use metadata, play/skip history or pre-defined dynamic lists
- Dedicated Search, Folders and History pages
- Moods browsing and mood/BPM-based smart playlists for Navidrome and Subsonic libraries

## 🧩 Integrations

- Last.fm, Libre.fm and ListenBrainz scrobbling, with offline storage and automatic retries
- Private mode for temporarily pausing external activity
- Secure storage for server credentials and API secrets by default
- Casting support for UPnP and Chromecast

## 🖥️ Interface

- Fast, responsive and broadly customizable
- Theme support including custom themes
- Extensive keyboard shortcuts catalog
- Automatic updates for Windows and macOS builds
- Easy built-in log viewing and exporting (privacy-conscious)
- Type to search
- Can run in the background or set to launch minimized, which uses the GNOME portal when available
- System tray integration

## Building locally

Start by cloning the repository.

```bash
git clone https://github.com/kmarius/Rufin.git
cd Rufin
```

Building Rufin requires Rust 1.96 or newerGTK 4.20 or newer, libadwaita 1.9 or newer, and GStreamer 1.26 or
newer.

**Arch Linux:**

```bash
sudo pacman -S --needed \
  base-devel rust cargo cmake ninja just pkgconf gettext gtk4 libadwaita \
  gstreamer gst-plugins-base gst-plugins-good gst-plugins-bad \
  gst-plugins-ugly gst-libav
```

For additional codecs, install the matching GStreamer plugins.

**Building:** After you installed dependencies for your operating system, you can build and run:

```bash
cargo build
cargo run
```

Local builds are shown as `Rufin (Development)` and use the isolated `Rufin.Devel` application
identity.

# Credits

Built with [GTK 4](https://www.gtk.org/), [libadwaita](https://gitlab.gnome.org/GNOME/libadwaita/), [gtk-rs](https://gtk-rs.org/) and [GStreamer](https://gstreamer.freedesktop.org/)

Rufin is greatly influenced by [Feishin](https://github.com/jeffvli/feishin), and a lot of design decisions are directly borrowed; as much we can achieve natively.

Player backend design and Smart Playlists are inspired from [Strawberry](https://github.com/strawberrymusicplayer/strawberry) (fork of [Clementine](https://github.com/clementine-player/Clementine)).

Icon is designed by [Commenter25](https://commenter.cc) and it is licensed under CC-BY-SA-3.0.

# License

[LICENSE](LICENSE)

[^codecs]:AAC and ALAC (including M4A/MP4), AIFF and WAV, APE, FLAC, TTA and WavPack, MP1/MP2/MP3 and Musepack, Ogg Vorbis/Opus/Speex, MKA and audio-only WebM, WMA/ASF, DSD, tracker modules such as MOD/XM/IT/S3M/MPTM, and emulated game music such as NSF/VGM. All supported packages have the required plugins. You may need additional GStreamer plugins if you are building Rufin natively; after installing them, restart Rufin and Resync the local library to retry rejected files.

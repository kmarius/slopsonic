# Slopsonic (working title)

Slopsonic is a native desktop music player for Navidrome and other OpenSubsonic-compatible servers. It manages a single remote library and currently targets Linux only.

It is a stripped-down fork of [Rufin](https://github.com/screwys/Rufin) and is currently written entirely by LLMs. The main goal right now is to make it maintainable by shedding features, restructuring it and then polishing the core functionality.

## Building locally

Start by cloning the repository.

```bash
git clone https://github.com/kmarius/slopsonic.git
cd slopsonic
```

Building Slopsonic requires Rust 1.96 or newer, GTK 4.20 or newer, libadwaita 1.9 or newer, and GStreamer 1.26 or
newer.

**Arch Linux:**

```bash
sudo pacman -S --needed \
  base-devel rust cargo pkgconf gtk4 libadwaita \
  gstreamer gst-plugins-base gst-plugins-good gst-plugins-bad \
  gst-plugins-ugly gst-libav
```

For additional codecs, install the matching GStreamer plugins.

**Building:** After you installed dependencies for your operating system, you can build and run:

```bash
cargo build
cargo run
```

Local builds are shown as `Slopsonic (Development)` and use the isolated `slopsonic.devel` application
identity.

# Credits

Built with [GTK 4](https://www.gtk.org/), [libadwaita](https://gitlab.gnome.org/GNOME/libadwaita/), [gtk-rs](https://gtk-rs.org/) and [GStreamer](https://gstreamer.freedesktop.org/)

Slopsonic is a fork of [Rufin](https://github.com/screwys/Rufin) and greatly influenced by [Feishin](https://github.com/jeffvli/feishin), and a lot of design decisions are directly borrowed; as much we can achieve natively.

Player backend design is inspired from [Strawberry](https://github.com/strawberrymusicplayer/strawberry) (fork of [Clementine](https://github.com/clementine-player/Clementine)).

Icon is designed by [Commenter25](https://commenter.cc) and it is licensed under CC-BY-SA-3.0.

# License

[LICENSE](LICENSE)

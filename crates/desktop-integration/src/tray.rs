#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrayIntent {
    Present,
    PlayPause,
    PreviousTrack,
    NextTrack,
    Quit,
}

mod freedesktop {
    use ksni::blocking::TrayMethods;
    use std::sync::OnceLock;
    use std::sync::mpsc::{Receiver, Sender, channel};
    use tracing::warn;

    use app_identity::{APP_ID, DISPLAY_NAME, STABLE_APP_ID};

    use super::TrayIntent;

    const TRAY_ICON_SIZES: [i32; 5] = [16, 22, 24, 32, 48];
    const APP_ICON_BYTES: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../resources/icons/hicolor/512x512/apps/io.github.kmarius.slopsonic.png"
    ));

    pub struct Tray {
        handle: ksni::blocking::Handle<RufinTray>,
    }

    #[derive(Clone)]
    pub(crate) struct RufinTray {
        sender: Sender<TrayIntent>,
        show_label: String,
        play_pause_label: String,
        previous_label: String,
        next_label: String,
        quit_label: String,
    }

    impl RufinTray {
        fn new(sender: Sender<TrayIntent>) -> Self {
            Self {
                sender,
                show_label: "Show Slopsonic".to_string(),
                play_pause_label: "Play/Pause".to_string(),
                previous_label: "Previous Track".to_string(),
                next_label: "Next Track".to_string(),
                quit_label: "Quit".to_string(),
            }
        }

        fn send_command(&self, command: TrayIntent) {
            let _ = self.sender.send(command);
        }
    }

    impl ksni::Tray for RufinTray {
        fn id(&self) -> String {
            APP_ID.to_string()
        }

        fn title(&self) -> String {
            DISPLAY_NAME.to_string()
        }

        fn icon_name(&self) -> String {
            if tray_icon_pixmaps().is_empty() {
                STABLE_APP_ID.to_string()
            } else {
                String::new()
            }
        }

        fn icon_pixmap(&self) -> Vec<ksni::Icon> {
            tray_icon_pixmaps().clone()
        }

        fn tool_tip(&self) -> ksni::ToolTip {
            ksni::ToolTip {
                icon_name: self.icon_name(),
                icon_pixmap: tray_icon_pixmaps().clone(),
                title: self.title(),
                description: String::new(),
            }
        }

        fn activate(&mut self, _x: i32, _y: i32) {
            self.send_command(TrayIntent::Present);
        }

        fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
            use ksni::menu::{MenuItem, StandardItem};

            vec![
                StandardItem {
                    label: self.show_label.clone(),
                    activate: Box::new(|tray: &mut RufinTray| {
                        tray.send_command(TrayIntent::Present)
                    }),
                    ..Default::default()
                }
                .into(),
                MenuItem::Separator,
                StandardItem {
                    label: self.play_pause_label.clone(),
                    activate: Box::new(|tray: &mut RufinTray| {
                        tray.send_command(TrayIntent::PlayPause)
                    }),
                    ..Default::default()
                }
                .into(),
                StandardItem {
                    label: self.previous_label.clone(),
                    activate: Box::new(|tray: &mut RufinTray| {
                        tray.send_command(TrayIntent::PreviousTrack)
                    }),
                    ..Default::default()
                }
                .into(),
                StandardItem {
                    label: self.next_label.clone(),
                    activate: Box::new(|tray: &mut RufinTray| {
                        tray.send_command(TrayIntent::NextTrack)
                    }),
                    ..Default::default()
                }
                .into(),
                MenuItem::Separator,
                StandardItem {
                    label: self.quit_label.clone(),
                    activate: Box::new(|tray: &mut RufinTray| tray.send_command(TrayIntent::Quit)),
                    ..Default::default()
                }
                .into(),
            ]
        }
    }

    impl Tray {
        pub fn start() -> Result<(Self, Receiver<TrayIntent>), String> {
            let (sender, receiver) = channel();
            let tray = RufinTray::new(sender);
            let handle = tray.disable_dbus_name(true).spawn().map_err(|error| {
                format!("failed to create status notifier tray item: {error:?}")
            })?;
            Ok((Self { handle }, receiver))
        }

        pub fn shutdown(self) {
            // KSNI queues shutdown before returning its completion awaiter.
            let _ = self.handle.shutdown();
        }
    }

    fn tray_icon_pixmaps() -> &'static Vec<ksni::Icon> {
        static ICONS: OnceLock<Vec<ksni::Icon>> = OnceLock::new();
        ICONS.get_or_init(build_tray_icon_pixmaps)
    }

    fn build_tray_icon_pixmaps() -> Vec<ksni::Icon> {
        let source = match artwork::decode_rgba(APP_ICON_BYTES, u32::MAX) {
            Ok(source) => source,
            Err(error) => {
                warn!(%error, "failed to load tray icon pixmap");
                return Vec::new();
            }
        };

        TRAY_ICON_SIZES
            .iter()
            .filter_map(|size| {
                source
                    .resized_exact(u32::try_from(*size).ok()?, u32::try_from(*size).ok()?)
                    .ok()
                    .and_then(|image| tray_icon_from_rgba(&image))
            })
            .collect()
    }

    fn tray_icon_from_rgba(image: &artwork::RgbaImage) -> Option<ksni::Icon> {
        let width = i32::try_from(image.width()).ok()?;
        let height = i32::try_from(image.height()).ok()?;
        let rowstride = usize::try_from(image.row_stride()).ok()?;
        let width_usize = usize::try_from(width).ok()?;
        let height_usize = usize::try_from(height).ok()?;
        let pixels = image.rgba();
        let mut data = Vec::with_capacity(width_usize * height_usize * 4);
        for y in 0..height_usize {
            let row = y.checked_mul(rowstride)?;
            for x in 0..width_usize {
                let offset = row.checked_add(x.checked_mul(4)?)?;
                let end = offset.checked_add(4)?;
                let pixel = pixels.get(offset..end)?;
                data.extend_from_slice(&[pixel[3], pixel[0], pixel[1], pixel[2]]);
            }
        }
        Some(ksni::Icon {
            width,
            height,
            data,
        })
    }

    #[cfg(test)]
    mod tests {
        use super::{RufinTray, TRAY_ICON_SIZES, tray_icon_pixmaps};
        use crate::tray::TrayIntent;
        use ksni::Tray;
        use ksni::menu::MenuItem;
        use std::sync::mpsc::channel;

        #[test]
        fn tray_use_controls() {
            let (sender, _receiver) = channel();
            let tray = RufinTray::new(sender);
            let items = tray.menu();
            let labels = standard_items(&items)
                .iter()
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>();

            assert_eq!(
                labels,
                vec![
                    "Show Slopsonic",
                    "Play/Pause",
                    "Previous Track",
                    "Next Track",
                    "Quit"
                ]
            );
            assert!(
                standard_items(&items)
                    .iter()
                    .all(|item| item.icon_name.is_empty() && item.shortcut.is_empty())
            );
        }

        #[test]
        fn tray_playback_command() {
            let (sender, receiver) = channel();
            let mut tray = RufinTray::new(sender);
            let mut items = tray.menu();

            activate_standard_item(&mut items, &mut tray, "Play/Pause");
            activate_standard_item(&mut items, &mut tray, "Previous Track");
            activate_standard_item(&mut items, &mut tray, "Next Track");

            assert_eq!(receiver.recv().ok(), Some(TrayIntent::PlayPause));
            assert_eq!(receiver.recv().ok(), Some(TrayIntent::PreviousTrack));
            assert_eq!(receiver.recv().ok(), Some(TrayIntent::NextTrack));
        }

        #[test]
        fn tray_icon_size() {
            let sizes = tray_icon_pixmaps()
                .iter()
                .map(|icon| (icon.width, icon.height))
                .collect::<Vec<_>>();

            assert_eq!(
                sizes,
                TRAY_ICON_SIZES
                    .iter()
                    .map(|size| (*size, *size))
                    .collect::<Vec<_>>()
            );
        }

        fn standard_items<T>(items: &[MenuItem<T>]) -> Vec<&ksni::menu::StandardItem<T>> {
            items
                .iter()
                .filter_map(|item| match item {
                    MenuItem::Standard(item) => Some(item),
                    _ => None,
                })
                .collect()
        }

        fn activate_standard_item(
            items: &mut [MenuItem<RufinTray>],
            tray: &mut RufinTray,
            label: &str,
        ) {
            let item = items
                .iter_mut()
                .find_map(|item| match item {
                    MenuItem::Standard(item) if item.label == label => Some(item),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("missing tray item {label}"));
            (item.activate)(tray);
        }
    }
}

pub use freedesktop::Tray;

use std::rc::Rc;

use adw::prelude::*;
use slopsonic_core::runtime::source::{ServerOperation, ServerProgressStage, source_download_text};
use slopsonic_core::runtime::{CacheUsage, ProductHandles, StorageSummary};

use crate::preferences::Preferences;

struct CacheRow {
    count: gtk::glib::WeakRef<gtk::Label>,
    size: gtk::glib::WeakRef<gtk::Label>,
    clear: gtk::glib::WeakRef<gtk::Button>,
}

struct StorageView {
    page: gtk::glib::WeakRef<adw::PreferencesPage>,
    total: gtk::glib::WeakRef<gtk::Label>,
    spinner: gtk::glib::WeakRef<gtk::Spinner>,
    error: gtk::glib::WeakRef<gtk::Label>,
    unavailable: gtk::glib::GString,
    artwork: CacheRow,
    products: ProductHandles,
}

pub(super) fn page(
    shell: &Rc<Preferences>,
    navigation_controls: &super::PreferencesNavigationControls,
) -> gtk::Widget {
    let resource = crate::ui_resource::STORAGE_PREFERENCES_RESOURCE;
    let builder = gtk_widgets::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, {
        navigation: adw::NavigationView,
        storage_page: adw::PreferencesPage,
        artwork_download: adw::ComboRow,
        total_size: gtk::Label,
        spinner: gtk::Spinner,
        error: gtk::Label,
        unavailable_text: gtk::Label,
        artwork_count: gtk::Label, artwork_size: gtk::Label, clear_artwork: gtk::Button,
    });
    navigation_controls.set_navigation(&navigation);
    navigation_controls.set_nested_page_visible(false);
    let controls = navigation_controls.clone();
    navigation.connect_popped(move |_, _| controls.set_nested_page_visible(false));
    let settings = shell.settings.persistence.load();
    artwork_download.set_selected(u32::from(
        settings.application.download_artwork_automatically,
    ));
    let settings = Rc::clone(&shell.settings);
    artwork_download.connect_selected_notify(move |row| {
        settings.set_app_setting(
            "automatic artwork download",
            row.selected() == 1,
            |settings| &mut settings.application.download_artwork_automatically,
        );
    });

    let view = Rc::new(StorageView {
        page: storage_page.downgrade(),
        total: total_size.downgrade(),
        spinner: spinner.downgrade(),
        error: error.downgrade(),
        unavailable: unavailable_text.label(),
        artwork: CacheRow {
            count: artwork_count.downgrade(),
            size: artwork_size.downgrade(),
            clear: clear_artwork.downgrade(),
        },
        products: shell.products.clone(),
    });
    let clicked = view.clone();
    clear_artwork.connect_clicked(move |_| clicked.confirm_clear());
    let mut operation = shell.products.source.operation();
    let downloads = Rc::downgrade(&view);
    gtk::glib::spawn_future_local(async move {
        let mut downloading = false;
        loop {
            let Some(view) = downloads.upgrade() else {
                break;
            };
            downloading = view.show_download(&operation.borrow_and_update(), downloading);
            drop(view);
            if operation.changed().await.is_err() {
                break;
            }
        }
    });
    storage_page.connect_map(move |_| view.load(false));
    navigation.upcast()
}

impl StorageView {
    fn confirm_clear(self: &Rc<Self>) {
        let view = self.clone();
        gtk::glib::spawn_future_local(async move {
            let Some(page) = view.page.upgrade() else {
                return;
            };
            let resource = crate::ui_resource::STORAGE_PREFERENCES_RESOURCE;
            let builder = gtk_widgets::ui_resource::builder(resource);
            let dialog: adw::AlertDialog =
                gtk_widgets::ui_resource::object(&builder, resource, "cache_entry_confirmation");
            if dialog.choose_future(Some(&page)).await == "clear" {
                view.load(true);
            }
        });
    }

    fn load(self: &Rc<Self>, clear: bool) {
        if self
            .spinner
            .upgrade()
            .is_some_and(|spinner| spinner.is_spinning())
        {
            return;
        }
        self.set_busy(true);
        let view = self.clone();
        let products = self.products.clone();
        let task = self
            .products
            .runtime
            .spawn(async move { products.storage_summary(clear).await });
        let cancel = task.abort_handle();
        let teardown = if clear {
            None
        } else {
            self.page
                .upgrade()
                .map(|page| page.connect_destroy(move |_| cancel.abort()))
        };
        gtk::glib::spawn_future_local(async move {
            let result = task.await;
            let Some(page) = view.page.upgrade() else {
                return;
            };
            if let Some(teardown) = teardown {
                page.disconnect(teardown);
            }
            let summary = result.unwrap_or_else(|error| StorageSummary {
                artwork: None,
                errors: vec![error.to_string()],
            });
            view.set_busy(false);
            view.artwork.update(summary.artwork, &view.unavailable);
            if let Some(label) = view.total.upgrade() {
                let size = summary.artwork.map_or_else(
                    || view.unavailable.clone(),
                    |usage| gtk::glib::format_size(usage.bytes),
                );
                label.set_label(&size);
            }
            if let Some(label) = view.error.upgrade() {
                label.set_label(&summary.errors.join("\n"));
                label.set_visible(!summary.errors.is_empty());
            }
        });
    }

    /// Shows the downloader's own count; the summary is measured again only once it ends.
    fn show_download(self: &Rc<Self>, operation: &ServerOperation, downloading: bool) -> bool {
        let ServerOperation::Downloading { progress, .. } = operation else {
            if downloading && self.page.upgrade().is_some_and(|page| page.is_mapped()) {
                self.load(false);
            }
            return false;
        };
        if progress.stage != ServerProgressStage::Artwork {
            return true;
        }
        if let Some(count) = self.artwork.count.upgrade() {
            count.set_label(&source_download_text(progress));
        }
        true
    }

    fn set_busy(&self, busy: bool) {
        if let Some(spinner) = self.spinner.upgrade() {
            spinner.set_spinning(busy);
            spinner.set_visible(busy);
        }
        if let Some(button) = self.artwork.clear.upgrade() {
            button.set_sensitive(!busy);
        }
    }
}

impl CacheRow {
    fn update(&self, usage: Option<CacheUsage>, unavailable: &str) {
        if let Some(count) = self.count.upgrade() {
            let text = usage.map_or_else(
                || unavailable.to_owned(),
                |usage| slopsonic_core::text::count_text(usage.entries, "entry", "entries"),
            );
            count.set_label(&text);
        }
        if let Some(size) = self.size.upgrade() {
            let text = usage.map_or_else(
                || unavailable.into(),
                |usage| gtk::glib::format_size(usage.bytes),
            );
            size.set_label(&text);
        }
        if let Some(button) = self.clear.upgrade() {
            button.set_sensitive(usage.is_none_or(|usage| usage.entries > 0));
        }
    }
}

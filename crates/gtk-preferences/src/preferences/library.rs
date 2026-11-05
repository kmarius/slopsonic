use gtk_widgets::forms::row_action_content;
use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use downloads::{DownloadQueueState, DownloadRule};
use gtk::gio;

use super::PreferencesNavigationControls;
use crate::Preferences;
use gtk_widgets::source_labels::configured_source_display_name;
use rufin_core::runtime::source::{SourceCacheSummary, SourceSummary};
use rufin_core::text::{album_count_text, track_count_text};

const DOWNLOAD_JOB_DRAG_PREFIX: &str = "rufin-download-job:";
const DEFAULT_DOWNLOAD_DIRECTORY_SUBTITLE: &str = "Default";

pub(super) fn library_page(
    shell: &Rc<Preferences>,
    dialog: &adw::Dialog,
    navigation_controls: &PreferencesNavigationControls,
) -> gtk::Widget {
    let navigation = adw::NavigationView::new();
    navigation_controls.set_navigation(&navigation);
    navigation_controls.set_nested_page_visible(false);
    let page = library_sources_page(shell, dialog, &navigation, navigation_controls);
    let root = adw::NavigationPage::new(&page, "Library");
    navigation.push(&root);
    let shell_for_pop = Rc::clone(shell);
    navigation.connect_popped(move |_, _| {
        shell_for_pop.release_inactive_add_server_form();
    });
    navigation.upcast::<gtk::Widget>()
}

fn library_sources_page(
    shell: &Rc<Preferences>,
    dialog: &adw::Dialog,
    navigation: &adw::NavigationView,
    navigation_controls: &PreferencesNavigationControls,
) -> adw::PreferencesPage {
    let resource = crate::ui_resource::LIBRARY_PREFERENCES_RESOURCE;
    let builder = gtk_widgets::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, {
        library_page: adw::PreferencesPage,
        listenbrainz: gtk::ToggleButton, jsonl: gtk::ToggleButton,
        activity_export: gtk::Button,
        servers_group: adw::PreferencesGroup,
        empty_remote_row: adw::ActionRow,
        add_source: gtk::Button,
    });

    let configured = shell.source.configured.borrow().clone();

    if configured.is_none() {
        servers_group.add(&empty_remote_row);
    } else {
        for status in configured.iter() {
            let server = &status.source;
            let summary = Some(&status.cache);
            let credentials = shell
                .products
                .source
                .editable(server.id.clone())
                .ok()
                .flatten()
                .map(|saved| saved.credentials);
            let row = adw::ActionRow::builder()
                .title(configured_source_display_name(server))
                .subtitle(source_summary_subtitle(
                    server,
                    summary,
                    credentials
                        .as_ref()
                        .map(|credentials| credentials.server_url.as_str()),
                    credentials
                        .as_ref()
                        .map(|credentials| credentials.username.as_str()),
                ))
                .subtitle_lines(4)
                .build();
            row.add_suffix(&gtk::Image::from_icon_name("rufin-go-next-symbolic"));
            row.set_activatable(true);
            let settings_shell = Rc::clone(shell);
            let navigation = navigation.downgrade();
            let navigation_controls = navigation_controls.clone();
            let dialog = dialog.downgrade();
            let server = server.clone();
            row.connect_activated(move |_| {
                let Some(dialog) = dialog.upgrade() else {
                    return;
                };
                let Some(navigation) = navigation.upgrade() else {
                    return;
                };
                let navigation_controls_for_close = navigation_controls.clone();
                let on_close: Rc<dyn Fn()> = Rc::new(move || {
                    navigation_controls_for_close.set_nested_page_visible(false);
                });
                let page = crate::preferences::source::manage::manage_server_navigation_page(
                    &settings_shell,
                    server.clone(),
                    &navigation,
                    &dialog,
                    on_close,
                );
                navigation.push(&page);
                navigation_controls.set_nested_page_visible(true);
            });
            servers_group.add(&row);
        }
    }

    let add_shell = Rc::clone(shell);
    let add_navigation = navigation.downgrade();
    let add_navigation_controls = navigation_controls.clone();
    let add_server_dialog = dialog.downgrade();
    add_source.connect_clicked(move |_| {
        let Some(add_server_dialog) = add_server_dialog.upgrade() else {
            return;
        };
        let Some(add_navigation) = add_navigation.upgrade() else {
            return;
        };
        let page = add_shell.add_server_navigation_page(&add_server_dialog);
        add_navigation.push(&page);
        add_navigation_controls.set_nested_page_visible(true);
    });
    add_source.set_visible(configured.is_none());
    servers_group.set_header_suffix(Some(&add_source));

    let weak = Rc::downgrade(shell);
    let (listenbrainz, jsonl) = (listenbrainz.downgrade(), jsonl.downgrade());
    activity_export.connect_clicked(move |_| {
        if let Some(shell) = weak.upgrade() {
            let format = if jsonl.upgrade().is_some_and(|button| button.is_active()) {
                None
            } else if listenbrainz
                .upgrade()
                .is_some_and(|button| button.is_active())
            {
                Some(library::ActivityCsvFormat::ListenBrainz)
            } else {
                Some(library::ActivityCsvFormat::LastFm)
            };
            (shell.effects)(crate::Effect::ExportActivity(format, None));
        }
    });
    library_page
}

pub(super) fn downloads_page(
    shell: &Rc<Preferences>,
    dialog: &adw::Dialog,
) -> adw::PreferencesPage {
    let resource = crate::ui_resource::LIBRARY_PREFERENCES_RESOURCE;
    let builder = gtk_widgets::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, {
        downloads_page: adw::PreferencesPage,
        downloads_group: adw::PreferencesGroup,
        empty_remote_row: adw::ActionRow,
    });
    let Some(source) = shell
        .source
        .configured
        .borrow()
        .as_ref()
        .map(|status| status.source.clone())
    else {
        downloads_group.add(&empty_remote_row);
        downloads_page.add(&downloads_group);
        return downloads_page;
    };
    downloads_page.add(&source_downloads_group(shell, dialog, &source));
    downloads_page
}

fn source_downloads_group(
    shell: &Rc<Preferences>,
    dialog: &adw::Dialog,
    server: &SourceSummary,
) -> adw::PreferencesGroup {
    let resource = crate::ui_resource::LIBRARY_PREFERENCES_RESOURCE;
    let builder = gtk_widgets::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, {
        downloads_group: adw::PreferencesGroup,
        download_folder_row: adw::ActionRow,
        open_folder: gtk::Button,
        reset_folder: gtk::Button,
        choose_folder: gtk::Button,
        downloaded_badge: adw::SwitchRow,
        download_actions_row: adw::PreferencesRow,
        download_action_buttons: gtk::Box,
        remove_all: gtk::Button,
        download_queue_header: adw::PreferencesRow,
        pause_downloads: gtk::Button,
    });
    let download_settings = shell
        .settings
        .current
        .borrow()
        .download_settings(&server.id);
    download_folder_row.set_subtitle(
        &download_settings
            .directory
            .as_ref()
            .map(|directory| {
                directory
                    .native()
                    .map(desktop_integration::display_path)
                    .unwrap_or_else(|| directory.to_string())
            })
            .unwrap_or_else(|| String::from(DEFAULT_DOWNLOAD_DIRECTORY_SUBTITLE)),
    );
    reset_folder.set_visible(download_settings.directory.is_some());
    let weak = Rc::downgrade(shell);
    let source_id = server.id.clone();
    open_folder.connect_clicked(move |_| {
        let Some(shell) = weak.upgrade() else { return };
        let directory = shell
            .settings
            .current
            .borrow()
            .download_directory(&source_id)
            .unwrap_or_else(|| shell.products.downloads.default_directory().to_path_buf());
        super::open_folder(&shell, gio::File::for_path(directory));
    });
    let choose_shell = Rc::clone(shell);
    let choose_source_id = server.id.clone();
    let choose_row = download_folder_row.downgrade();
    let choose_reset = reset_folder.downgrade();
    choose_folder.connect_clicked(move |_| {
        let shell = Rc::clone(&choose_shell);
        let source_id = choose_source_id.clone();
        let row = choose_row.clone();
        let reset = choose_reset.clone();
        gtk::glib::spawn_future_local(async move {
            let chooser = gtk::FileDialog::builder()
                .title("Select Download Folder".to_string())
                .build();
            let initial_directory = shell
                .settings
                .current
                .borrow()
                .download_directory(&source_id);
            if let Some(directory) = initial_directory.as_ref() {
                chooser.set_initial_folder(Some(&gio::File::for_path(directory)));
            }
            let Ok(folder) = chooser.select_folder_future(Some(&shell.window)).await else {
                return;
            };
            let Some(path) = folder.path() else {
                return;
            };
            if shell
                .settings
                .update_app_settings("download folder", |settings| {
                    settings.set_download_directory(source_id, Some(path.clone()))
                })
                .is_some()
            {
                if let Some(row) = row.upgrade() {
                    row.set_subtitle(&desktop_integration::display_path(&path));
                }
                if let Some(reset) = reset.upgrade() {
                    reset.set_visible(true);
                }
            }
        });
    });
    let reset_shell = Rc::clone(shell);
    let reset_source_id = server.id.clone();
    let reset_row = download_folder_row.downgrade();
    let reset_button = reset_folder.downgrade();
    reset_folder.connect_clicked(move |_| {
        let Some(reset_button) = reset_button.upgrade() else {
            return;
        };
        if reset_shell
            .settings
            .update_app_settings("reset download folder", |settings| {
                settings.set_download_directory(reset_source_id.clone(), None)
            })
            .is_some()
        {
            if let Some(row) = reset_row.upgrade() {
                row.set_subtitle(&String::from(DEFAULT_DOWNLOAD_DIRECTORY_SUBTITLE));
            }
            reset_button.set_visible(false);
        }
    });
    downloads_group.add(&download_folder_row);

    let quality_shell = Rc::clone(shell);
    let quality_source_id = server.id.clone();
    for row in crate::download_quality::rows(download_settings.quality, move |quality, _| {
        quality_shell
            .settings
            .update_app_settings("download quality", |settings| {
                settings.set_download_quality(quality_source_id.clone(), quality)
            });
    }) {
        downloads_group.add(&row);
    }
    downloaded_badge.set_active(
        shell
            .settings
            .current
            .borrow()
            .application
            .show_downloaded_badges,
    );
    let downloaded_badge_shell = Rc::clone(shell);
    downloaded_badge.connect_active_notify(move |row| {
        downloaded_badge_shell
            .downloads
            .set_downloaded_badges_visible(row.is_active());
    });
    downloads_group.add(&downloaded_badge);
    let add_rule = add_download_rules(
        &downloads_group,
        shell,
        dialog,
        &server.id,
        download_settings.rules,
    );

    download_action_buttons.prepend(&add_rule);
    let remove_shell = Rc::clone(shell);
    let source_id = server.id.clone();
    let preferences_dialog = dialog.downgrade();
    remove_all.connect_clicked(move |_| {
        let Some(preferences_dialog) = preferences_dialog.upgrade() else {
            return;
        };
        confirm_remove_all_downloads(&remove_shell, &preferences_dialog, source_id.clone());
    });
    downloads_group.add(&download_actions_row);

    downloads_group.add(&download_queue_header);
    add_download_queue(&downloads_group, shell, &server.id, &pause_downloads);
    downloads_group
}

fn add_download_rules(
    group: &adw::PreferencesGroup,
    shell: &Rc<Preferences>,
    preferences_dialog: &adw::Dialog,
    source_id: &sources::SourceId,
    rules: rufin_core::settings::DownloadRules,
) -> gtk::MenuButton {
    let mut rows = Vec::new();
    for rule in DownloadRule::ALL {
        let row = adw::ActionRow::builder()
            .title(String::from(download_rule_title(rule)))
            .subtitle(String::from(download_rule_subtitle(rule)))
            .build();
        row.set_visible(rules.contains(rule));
        let artwork = gtk_widgets::downloads::download_rule_artwork(
            &shell.artwork,
            &shell.products.library,
            &shell.products.runtime,
            source_id.clone(),
            rule,
            48,
        );
        artwork.set_margin_top(6);
        artwork.set_margin_bottom(6);
        row.add_prefix(&artwork);
        let more = gtk::MenuButton::new();
        more.set_icon_name("rufin-view-more-symbolic");
        more.add_css_class("flat");
        more.set_valign(gtk::Align::Center);
        more.set_tooltip_text(Some("Rule actions"));
        let menu = gio::Menu::new();
        menu.append(
            Some("Remove Rule, Keep Downloads"),
            Some("download-rule.remove"),
        );
        menu.append(
            Some("Remove Rule and Delete Downloads"),
            Some("download-rule.delete"),
        );
        let popover = gtk::PopoverMenu::from_model(Some(&menu));
        more.set_popover(Some(&popover));
        let actions = gio::SimpleActionGroup::new();
        let remove = gio::SimpleAction::new("remove", None);
        let remove_shell = Rc::clone(shell);
        let remove_source_id = source_id.clone();
        let remove_row = row.downgrade();
        remove.connect_activate(move |_, _| {
            let Some(row) = remove_row.upgrade() else {
                return;
            };
            remove_download_rule(&remove_shell, remove_source_id.clone(), rule, false, &row);
        });
        actions.add_action(&remove);
        let delete = gio::SimpleAction::new("delete", None);
        let delete_shell = Rc::clone(shell);
        let delete_source_id = source_id.clone();
        let delete_row = row.downgrade();
        let delete_dialog = preferences_dialog.downgrade();
        delete.connect_activate(move |_, _| {
            let (Some(row), Some(dialog)) = (delete_row.upgrade(), delete_dialog.upgrade()) else {
                return;
            };
            confirm_remove_download_rule(
                &delete_shell,
                &dialog,
                delete_source_id.clone(),
                rule,
                row,
            );
        });
        actions.add_action(&delete);
        more.insert_action_group("download-rule", Some(&actions));
        row.add_suffix(&more);
        group.add(&row);
        rows.push((rule, row));
    }

    let menu = gio::Menu::new();
    let actions = gio::SimpleActionGroup::new();
    for (rule, row) in rows {
        let action_name = download_rule_action_name(rule);
        menu.append(
            Some(&String::from(download_rule_title(rule))),
            Some(&format!("download-rules.{action_name}")),
        );
        let action = gio::SimpleAction::new(action_name, None);
        let add_shell = Rc::clone(shell);
        let add_source_id = source_id.clone();
        let add_row = row.downgrade();
        action.connect_activate(move |_, _| {
            let Some(row) = add_row.upgrade() else {
                return;
            };
            if add_shell
                .settings
                .update_app_settings("add download rule", |settings| {
                    let mut rules = settings.download_rules(&add_source_id);
                    rules.set(rule, true);
                    settings.set_download_rules(add_source_id.clone(), rules)
                })
                .is_some()
                || add_shell
                    .settings
                    .current
                    .borrow()
                    .download_rules(&add_source_id)
                    .contains(rule)
            {
                row.set_visible(true);
            }
        });
        actions.add_action(&action);
    }
    let add = gtk::MenuButton::new();
    add.set_child(Some(&row_action_content(
        "New download rule",
        "rufin-list-add-symbolic",
    )));
    add.add_css_class("flat");
    add.set_halign(gtk::Align::Fill);
    add.set_hexpand(true);
    add.set_tooltip_text(Some("New download rule"));
    add.insert_action_group("download-rules", Some(&actions));
    add.set_popover(Some(&gtk::PopoverMenu::from_model(Some(&menu))));
    add
}

fn remove_download_rule(
    shell: &Rc<Preferences>,
    source_id: sources::SourceId,
    rule: DownloadRule,
    delete_downloads: bool,
    row: &adw::ActionRow,
) {
    let removed = shell
        .settings
        .update_app_settings("remove download rule", |settings| {
            let mut rules = settings.download_rules(&source_id);
            rules.set(rule, false);
            settings.set_download_rules(source_id.clone(), rules)
        })
        .is_some();
    if removed {
        shell
            .products
            .downloads
            .remove_rule(source_id, rule, delete_downloads);
        row.set_visible(false);
    }
}

fn confirm_remove_download_rule(
    shell: &Rc<Preferences>,
    preferences_dialog: &adw::Dialog,
    source_id: sources::SourceId,
    rule: DownloadRule,
    row: adw::ActionRow,
) {
    let dialog = adw::AlertDialog::builder()
        .heading("Remove Rule and Downloads".to_string())
        .body(
            "Only downloads from this rule will be deleted. Shared and manual downloads stay"
                .to_string(),
        )
        .build();
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("remove", "Remove and Delete");
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
    let shell = Rc::clone(shell);
    dialog.choose(
        Some(preferences_dialog),
        None::<&gio::Cancellable>,
        move |response| {
            if response.as_str() == "remove" {
                remove_download_rule(&shell, source_id.clone(), rule, true, &row);
            }
        },
    );
}

pub(super) fn download_rule_title(rule: DownloadRule) -> &'static str {
    match rule {
        DownloadRule::EntireLibrary => "Entire Library",
        DownloadRule::Favorites => "Favorites",
        DownloadRule::AllPlaylists => "All Playlists",
        DownloadRule::LatestFiveAlbums => "5 Latest Albums",
    }
}

pub(super) fn download_rule_subtitle(rule: DownloadRule) -> &'static str {
    match rule {
        DownloadRule::EntireLibrary => "Everything, including new tracks",
        DownloadRule::Favorites => "Favorite tracks, plus tracks from favorite albums and artists",
        DownloadRule::AllPlaylists => "Tracks from every playlist",
        DownloadRule::LatestFiveAlbums => "Tracks from your five latest albums",
    }
}

fn download_rule_action_name(rule: DownloadRule) -> &'static str {
    match rule {
        DownloadRule::EntireLibrary => "add-entire-library",
        DownloadRule::Favorites => "add-favorites",
        DownloadRule::AllPlaylists => "add-all-playlists",
        DownloadRule::LatestFiveAlbums => "add-latest-five-albums",
    }
}

fn add_download_queue(
    group: &adw::PreferencesGroup,
    shell: &Rc<Preferences>,
    source_id: &sources::SourceId,
    pause: &gtk::Button,
) {
    let resource = crate::ui_resource::DOWNLOAD_QUEUE_RESOURCE;
    let builder = gtk_widgets::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, { scroll: gtk::ScrolledWindow, queue: gtk::ListView, empty: adw::ActionRow });
    use gtk_widgets::sparse_model::{
        SparseRouteModel, SparseSource, bind_sparse_item, item_at_from_item, unbind_sparse_item,
    };
    let owner = shell.products.downloads.clone();
    let source = source_id.clone();
    let jobs = SparseRouteModel::<String, downloads::DownloadQueueItem>::new(
        SparseSource::Query { count: 0 },
        64,
        shell.products.runtime.clone(),
        Rc::new(move |_, range, _| {
            let owner = owner.clone();
            let source_id = source.clone();
            Box::pin(async move {
                owner
                    .queue_page(
                        downloads::DownloadQueueScope::Source { source_id },
                        range.start as u64,
                        range.len() as u32,
                    )
                    .await
                    .map(|page| page.jobs)
                    .map_err(|error| error.to_string())
            })
        }),
    );
    queue.set_model(Some(&gtk::NoSelection::new(Some(jobs.list_model()))));
    let factory = gtk::SignalListItemFactory::new();
    let weak_shell = Rc::downgrade(shell);
    let source = source_id.clone();
    factory.connect_setup(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let resource = crate::ui_resource::DOWNLOAD_QUEUE_ROW_RESOURCE;
        let builder = gtk_widgets::ui_resource::builder(resource);
        gtk_widgets::objects!(builder, resource, {
            row: adw::ActionRow, artwork: gtk::Box, drag: gtk::Image,
            up: gtk::Button, down: gtk::Button, cancel: gtk::Button, clear: gtk::Button,
        });
        let current = Rc::new(RefCell::new(None::<downloads::DownloadQueueItem>));
        for (button, after) in [(&up, false), (&down, true)] {
            let weak = weak_shell.clone();
            let item = item.downgrade();
            let source = source.clone();
            button.connect_clicked(move |_| {
                let (Some(shell), Some(item)) = (weak.upgrade(), item.upgrade()) else {
                    return;
                };
                let Some(job) = item_at_from_item::<downloads::DownloadQueueItem>(&item) else {
                    return;
                };
                shell.products.downloads.move_job(
                    source.clone(),
                    job.id,
                    if after {
                        downloads::DownloadQueueMove::Down
                    } else {
                        downloads::DownloadQueueMove::Up
                    },
                );
            });
        }
        for (button, remove_files) in [(cancel, false), (clear, true)] {
            let weak = weak_shell.clone();
            let current = current.clone();
            button.connect_clicked(move |_| {
                let Some(shell) = weak.upgrade() else { return };
                let current = current.borrow();
                let Some(job) = current.as_ref() else { return };
                let Some(source) = job.source_id.clone() else {
                    return;
                };
                if remove_files {
                    shell.products.downloads.clear_job(source, job.id.clone());
                } else {
                    shell.products.downloads.cancel(source, job.id.clone());
                }
            });
        }
        let drag_source = gtk::DragSource::builder()
            .actions(gtk::gdk::DragAction::MOVE)
            .build();
        let drag_job = current.clone();
        drag_source.connect_prepare(move |_, _, _| {
            let current = drag_job.borrow();
            current.as_ref().map(|job| {
                gtk::gdk::ContentProvider::for_value(
                    &format!("{DOWNLOAD_JOB_DRAG_PREFIX}{}", job.id).to_value(),
                )
            })
        });
        drag.add_controller(drag_source);
        let target = gtk::DropTarget::new(String::static_type(), gtk::gdk::DragAction::MOVE);
        let weak = weak_shell.clone();
        let drop_job = current.clone();
        let drop_row = row.downgrade();
        target.connect_drop(move |_, value, _, y| {
            let (Some(shell), Some(row), Ok(id)) =
                (weak.upgrade(), drop_row.upgrade(), value.get::<String>())
            else {
                return false;
            };
            let Some(id) = id.strip_prefix(DOWNLOAD_JOB_DRAG_PREFIX) else {
                return false;
            };
            let Some(job) = drop_job.borrow().clone() else {
                return false;
            };
            let Some(source) = job.source_id else {
                return false;
            };
            if id == job.id {
                return false;
            }
            let movement = if y > f64::from(row.height()) / 2.0 {
                downloads::DownloadQueueMove::After { job_id: job.id }
            } else {
                downloads::DownloadQueueMove::Before { job_id: job.id }
            };
            shell
                .products
                .downloads
                .move_job(source, id.into(), movement);
            true
        });
        row.add_controller(target);
        item.set_child(Some(&row));
        let weak = weak_shell.clone();
        let source = source.clone();
        row.set_sensitive(false);
        let row = row.downgrade();
        let artwork = artwork.downgrade();
        let up = up.downgrade();
        let down = down.downgrade();
        let refresh: Rc<dyn Fn(&gtk::ListItem)> = Rc::new(move |item| {
            let (Some(shell), Some(row), Some(artwork), Some(up), Some(down)) = (
                weak.upgrade(),
                row.upgrade(),
                artwork.upgrade(),
                up.upgrade(),
                down.upgrade(),
            ) else {
                return;
            };
            let position = item.position() as usize;
            let job = item_at_from_item::<downloads::DownloadQueueItem>(item);
            row.set_sensitive(job.is_some());
            let Some(job) = job else {
                current.borrow_mut().take();
                while let Some(child) = artwork.first_child() {
                    artwork.remove(&child);
                }
                return;
            };
            let changed = current.borrow().as_ref().is_none_or(|old| {
                old.id != job.id
                    || old.subject != job.subject
                    || old.preview_uris != job.preview_uris
            });
            if changed {
                while let Some(child) = artwork.first_child() {
                    artwork.remove(&child);
                }
                artwork.append(&gtk_widgets::downloads::media_artwork(
                    &shell.artwork,
                    &shell.products.library,
                    &shell.products.runtime,
                    &job.preview_uris,
                    48,
                ));
                row.set_title(&gtk_widgets::downloads::download_subject_title(
                    &job.subject,
                ));
            }
            row.set_subtitle(&download_queue_item_subtitle(&job));
            up.set_sensitive(position > 0);
            down.set_sensitive(
                shell
                    .products
                    .downloads
                    .status()
                    .queues
                    .iter()
                    .find(|queue| queue.source_id.as_ref() == Some(&source))
                    .is_some_and(|queue| (position as u64 + 1) < queue.total_jobs),
            );
            *current.borrow_mut() = Some(job.clone());
        });
        let update = refresh.clone();
        item.connect_item_notify(move |item| bind_sparse_item(item, update.clone()));
        item.connect_position_notify(move |item| refresh(item));
    });
    factory.connect_unbind(|_, item| {
        unbind_sparse_item(item.downcast_ref::<gtk::ListItem>().unwrap())
    });
    queue.set_factory(Some(&factory));
    group.add(&scroll);
    group.add(&empty);
    let owner = shell.products.downloads.clone();
    pause.connect_clicked(move |_| owner.set_paused(!owner.status().paused));
    let task = Rc::new(RefCell::new(None::<gtk::glib::JoinHandle<()>>));
    let running = task.clone();
    let visible = group.downgrade();
    let pause = pause.downgrade();
    let scroll = scroll.downgrade();
    let empty = empty.downgrade();
    let owner = shell.products.downloads.clone();
    let refresh_source = source_id.clone();
    let source_id = source_id.clone();
    let rows = jobs.clone();
    let refresh: Rc<dyn Fn()> = Rc::new(move || {
        let (Some(group), Some(pause), Some(scroll), Some(empty)) = (
            visible.upgrade(),
            pause.upgrade(),
            scroll.upgrade(),
            empty.upgrade(),
        ) else {
            return;
        };
        if !group.is_mapped() {
            return;
        }
        let state = owner.status();
        let count = state
            .queues
            .iter()
            .find(|queue| queue.source_id.as_ref() == Some(&source_id))
            .map_or(0, |queue| queue.total_jobs as usize);
        pause.set_visible(count > 0);
        let (icon, label) = if state.paused {
            (
                "rufin-media-playback-start-symbolic",
                "Continue".to_string(),
            )
        } else {
            ("rufin-media-playback-pause-symbolic", "Pause".to_string())
        };
        pause.set_icon_name(icon);
        pause.set_tooltip_text(Some(&label));
        pause.update_property(&[gtk::accessible::Property::Label(&label)]);
        scroll.set_visible(count > 0);
        empty.set_visible(count == 0);
        if let Some(task) = running.borrow_mut().take() {
            task.abort();
        }
        let offset = (rows.first_live_position().unwrap_or_default() / 64 * 64)
            .min(count.saturating_sub(1) / 64 * 64);
        let owner = owner.clone();
        let source_id = source_id.clone();
        let rows = rows.clone();
        *running.borrow_mut() = Some(gtk::glib::spawn_future_local(async move {
            if let Ok(page) = owner
                .queue_page(
                    downloads::DownloadQueueScope::Source { source_id },
                    offset as u64,
                    128,
                )
                .await
            {
                rows.replace_prepared_at(
                    SparseSource::Query {
                        count: page.total as usize,
                    },
                    offset,
                    page.jobs,
                    Vec::new(),
                    |job| job.id.clone(),
                );
                rows.resume_initial_demand();
            }
        }));
    });
    let downloads = shell.downloads.clone();
    group.connect_map(move |_| {
        downloads.set_queue_refresh(refresh_source.clone(), &refresh);
        refresh();
    });
    group.connect_unmap(move |_| {
        if let Some(task) = task.borrow_mut().take() {
            task.abort();
        }
        jobs.cancel();
    });
}

fn download_queue_item_subtitle(job: &downloads::DownloadQueueItem) -> String {
    let progress = format!(
        "{} / {}",
        job.completed_tracks.min(job.total_tracks),
        job.total_tracks
    );
    let state = match job.state {
        DownloadQueueState::Queued => "Queued".to_string(),
        DownloadQueueState::Downloading => "Downloading".to_string(),
        DownloadQueueState::WaitingForConnection => "Waiting for connection".to_string(),
        DownloadQueueState::NeedsAttention => "Needs attention".to_string(),
    };
    let quality = crate::download_quality::label(job.quality);
    format!("{state} · {progress} · {quality}")
}

fn confirm_remove_all_downloads(
    shell: &Rc<Preferences>,
    preferences_dialog: &adw::Dialog,
    source_id: sources::SourceId,
) {
    let dialog = adw::AlertDialog::builder()
        .heading("Remove All Downloads".to_string())
        .body(
            "Downloads from this source will be removed, and automatic rules will be turned off"
                .to_string(),
        )
        .build();
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("remove", "Remove");
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
    let shell = Rc::clone(shell);
    let window = shell.window.clone();
    let preferences_dialog = preferences_dialog.downgrade();
    dialog.choose(Some(&window), None::<&gio::Cancellable>, move |response| {
        if response.as_str() == "remove" {
            let rules_are_empty = shell
                .settings
                .current
                .borrow()
                .download_rules(&source_id)
                .is_empty();
            let rules_disabled = rules_are_empty
                || shell
                    .settings
                    .update_app_settings("remove all downloads", |settings| {
                        settings
                            .set_download_rules(source_id.clone(), crate::DownloadRules::default())
                    })
                    .is_some();
            if rules_disabled {
                shell.products.downloads.clear(source_id.clone(), true);
                if let Some(preferences_dialog) = preferences_dialog.upgrade() {
                    preferences_dialog.close();
                }
            }
        }
    });
}

fn source_summary_subtitle(
    server: &SourceSummary,
    summary: Option<&SourceCacheSummary>,
    address: Option<&str>,
    username: Option<&str>,
) -> String {
    let address = address
        .filter(|address| !address.trim().is_empty())
        .unwrap_or_default()
        .to_string();
    let folder = summary
        .and_then(|summary| summary.selected_music_folder_name.as_deref())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| "All Music".to_string());
    let account = username
        .filter(|username| !username.trim().is_empty())
        .map(|username| format!("User: {username}"))
        .unwrap_or_default();
    let cache = summary.map(source_cache_line).unwrap_or_default();
    let provider_line = metadata_line([
        gtk_widgets::source_labels::source_kind_title(server.kind).to_string(),
        address,
    ]);
    let folder_line = metadata_line([account, format!("Music Folder: {folder}")]);
    let cache_line = metadata_line([cache]);
    [provider_line, folder_line, cache_line]
        .into_iter()
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn metadata_line(parts: impl IntoIterator<Item = String>) -> String {
    parts
        .into_iter()
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" - ")
}

fn source_cache_line(summary: &SourceCacheSummary) -> String {
    format!(
        "Cached: {}, {}",
        album_count_text(summary.album_count as u64),
        track_count_text(summary.track_count as u64)
    )
}

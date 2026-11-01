use gtk_widgets::source_labels::source_kind_title;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use rufin_core::runtime::source::SourceSummary;

use sources::SourceId;

use super::login::source_settings_group;
use crate::Preferences;
use gtk_widgets::layout::large_popup_content_width;

const MANAGE_SERVER_CLAMP_WIDTH: i32 = 560;

#[derive(Clone)]
struct ManageServerExitSlot {
    navigation: glib::WeakRef<adw::NavigationView>,
    on_close: Rc<dyn Fn()>,
}

pub fn manage_server_navigation_page(
    shell: &Rc<Preferences>,
    server: SourceSummary,
    navigation: &adw::NavigationView,
    preferences_dialog: &adw::Dialog,
    on_close: Rc<dyn Fn()>,
) -> adw::NavigationPage {
    let title = server_display_name(&server);
    let content = manage_server_content(
        shell,
        server,
        ManageServerExitSlot {
            navigation: navigation.downgrade(),
            on_close,
        },
        preferences_dialog,
    );
    adw::NavigationPage::new(&content, &title)
}

fn manage_server_content(
    shell: &Rc<Preferences>,
    server: SourceSummary,
    exit: ManageServerExitSlot,
    preferences_dialog: &adw::Dialog,
) -> gtk::Widget {
    let selected = shell.source.configured.borrow().selected_source_id.as_ref() == Some(&server.id);
    let resource = gtk_widgets::ui_resource::MANAGE_SERVER_RESOURCE;
    let builder = gtk_widgets::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, {
        scroller: gtk::ScrolledWindow,
        clamp: adw::Clamp,
        content: gtk::Box,
    });
    clamp.set_maximum_size(large_popup_content_width(MANAGE_SERVER_CLAMP_WIDTH));
    match shell.products.source.editable(server.id.clone()) {
        Ok(Some(saved)) => {
            if let Some(settings_group) = source_settings_group(shell, &saved) {
                let settings = match settings_group {
                    Ok(settings) => settings,
                    Err(error) => source_settings_error(&error),
                };
                content.append(&settings);
            }
        }
        Ok(None) => {}
        Err(error) => content.append(&source_settings_error(&error.to_string())),
    }

    if let Some(half_stars) = super::half_stars_row(shell, &server) {
        let library = adw::PreferencesGroup::builder()
            .title("Library".to_string())
            .build();
        library.add(&half_stars);
        content.append(&library);
    }

    content.append(&server_actions_group(
        shell,
        &server,
        selected,
        &exit,
        preferences_dialog,
    ));
    scroller.upcast()
}

fn source_settings_error(error: &str) -> gtk::Widget {
    let label = gtk::Label::new(Some(error));
    label.set_wrap(true);
    label.upcast()
}

fn close_manage_server(exit: &ManageServerExitSlot) {
    if let Some(navigation) = exit.navigation.upgrade() {
        navigation.pop();
    }
    (exit.on_close)();
}

fn server_actions_group(
    shell: &Rc<Preferences>,
    server: &SourceSummary,
    selected: bool,
    exit: &ManageServerExitSlot,
    preferences_dialog: &adw::Dialog,
) -> adw::PreferencesGroup {
    let resource = crate::ui_resource::SERVER_ACTIONS_RESOURCE;
    let builder = gtk_widgets::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, {
        group: adw::PreferencesGroup,
        select: gtk::Button,
        resync: gtk::Button,
        forget: gtk::Button,
    });

    if !selected {
        let source = shell.products.source.clone();
        let source_id = server.id.clone();
        let exit = exit.clone();
        let preferences_dialog = preferences_dialog.downgrade();
        select.connect_clicked(move |_| {
            let source = source.clone();
            let source_id = source_id.clone();
            gtk::glib::spawn_future_local(async move {
                let _ = source.select_source(source_id).await;
            });
            close_manage_server(&exit);
            if let Some(dialog) = preferences_dialog.upgrade() {
                dialog.close();
            }
        });
    } else {
        select.set_visible(false);
    }

    let source = shell.products.source.clone();
    let source_id = server.id.clone();
    let preferences_dialog_for_resync = preferences_dialog.downgrade();
    resync.connect_clicked(move |_| {
        let source = source.clone();
        let source_id = source_id.clone();
        gtk::glib::spawn_future_local(async move {
            let _ = source.refresh_source(source_id, false).await;
        });
        if let Some(dialog) = preferences_dialog_for_resync.upgrade() {
            dialog.close();
        }
    });
    let forget_shell = Rc::clone(shell);
    let source_id = server.id.clone();
    let server_name = server_display_name(server);
    let exit = exit.clone();
    let preferences_dialog = preferences_dialog.downgrade();
    forget.connect_clicked(move |_| {
        confirm_forget_source(
            &forget_shell,
            source_id.clone(),
            &server_name,
            Rc::new({
                let exit = exit.clone();
                let preferences_dialog = preferences_dialog.clone();
                move || {
                    close_manage_server(&exit);
                    if let Some(dialog) = preferences_dialog.upgrade() {
                        dialog.close();
                    }
                }
            }),
        );
    });
    group
}

pub fn confirm_forget_source(
    shell: &Rc<Preferences>,
    source_id: SourceId,
    server_name: &str,
    after_forget: Rc<dyn Fn()>,
) {
    let dialog = adw::AlertDialog::builder()
        .heading("Forget Source".to_string())
        .body(format!(
            "This removes the server, cached library metadata, queue snapshot, and saved token for {}",
            server_name
        ))
        .build();
    let cancel = "Cancel".to_string();
    let forget = "Forget Source".to_string();
    dialog.add_responses(&[("cancel", cancel.as_str()), ("forget", forget.as_str())]);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    dialog.set_response_appearance("forget", adw::ResponseAppearance::Destructive);
    let source = shell.products.source.clone();
    dialog.choose(
        Some(&shell.window),
        None::<&gio::Cancellable>,
        move |response| {
            if response.as_str() == "forget" {
                gtk::glib::spawn_future_local(async move {
                    let _ = source.forget_source(source_id).await;
                });
                after_forget();
            }
        },
    );
}

fn server_display_name(server: &SourceSummary) -> String {
    if server.name.trim().is_empty() {
        source_kind_title(&server.kind)
            .map(String::from)
            .unwrap_or_else(|| server.kind.clone())
    } else {
        server.name.clone()
    }
}

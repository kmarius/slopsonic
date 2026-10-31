use adw::prelude::*;
use std::{cell::RefCell, rc::Rc};

use crate::shell::Shell;
use ::library::PlaylistKey;

impl Shell {
    pub(crate) fn edit_playlist_dialog(
        self: &Rc<Self>,
        playlist_id: PlaylistKey,
        current_name: String,
    ) {
        let shell = Rc::clone(self);
        gtk::glib::spawn_future_local(async move {
            let resource = gtk_widgets::ui_resource::PLAYLIST_NAME_DIALOG_RESOURCE;
            let builder = gtk_widgets::ui_resource::builder(resource);
            let editing = match rufin_core::playlists::playlist_artwork_editing(
                &shell.products.source,
                playlist_id,
            )
            .recv()
            .await
            {
                Ok(Ok(editing)) => editing,
                Ok(Err(error)) => {
                    shell.control_feedback.show_feedback_toast(error);
                    return;
                }
                Err(_) => return,
            };
            let public =
                match rufin_core::playlists::playlist_public(&shell.products.source, playlist_id)
                    .recv()
                    .await
                {
                    Ok(Ok(public)) => public,
                    Ok(Err(error)) => {
                        shell.control_feedback.show_feedback_toast(error);
                        None
                    }
                    Err(_) => return,
                };
            gtk_widgets::objects!(builder, resource, {
                edit_dialog: adw::Dialog, name_entry: gtk::Entry,
                save_edit: gtk::Button, cancel_edit: gtk::Button,
                playlist_public: gtk::Switch, playlist_public_options: gtk::Box,
                editor_content: gtk::Box, editor_layout: adw::WrapBox,
                playlist_options: gtk::Box,
            });
            let artwork = gtk_editors::playlist_artwork::ArtworkEditor::new(
                &shell.products.source,
                &shell.artwork,
                editing,
                &editor_content,
                &save_edit,
                move |owner| rufin_core::playlists::regenerate_playlist_artwork(owner, playlist_id),
            );
            editor_layout.insert_child_after(&artwork.frame, None::<&gtk::Widget>);
            playlist_options.append(&artwork.status);
            let lifetime = RefCell::new(Some(Rc::clone(&artwork)));
            edit_dialog.connect_closed(move |_| {
                lifetime.borrow_mut().take();
            });
            playlist_public_options.set_visible(public.is_some());
            playlist_public.set_active(public.unwrap_or(false));
            let dialog = edit_dialog.downgrade();
            cancel_edit.connect_clicked(move |_| {
                if let Some(dialog) = dialog.upgrade() {
                    dialog.close();
                }
            });
            name_entry.set_text(&current_name);
            let weak_shell = Rc::downgrade(&shell);
            let dialog = edit_dialog.downgrade();
            let entry = name_entry.clone();
            let artwork = Rc::downgrade(&artwork);
            let public_toggle = playlist_public.clone();
            save_edit.connect_clicked(move |_| {
                let Some(shell) = weak_shell.upgrade() else {
                    return;
                };
                let name = entry.text().trim().to_owned();
                if name.is_empty() {
                    return;
                }
                let Some(editor) = artwork.upgrade() else {
                    return;
                };
                let change = editor.change();
                editor.set_busy(true);
                let artwork = artwork.clone();
                let dialog = dialog.clone();
                let changed_public = public
                    .filter(|value| *value != public_toggle.is_active())
                    .map(|_| public_toggle.is_active());
                let changed_name = name != current_name;
                gtk::glib::spawn_future_local(async move {
                    let result: Result<(), String> =
                        if change.is_some() || changed_name || changed_public.is_some() {
                            rufin_core::playlists::update_playlist(
                                &shell.products.source,
                                playlist_id,
                                changed_name.then_some(name),
                                changed_public,
                                change,
                            )
                            .await
                            .map(|_| ())
                            .map_err(|error| error.to_string())
                        } else {
                            Ok(())
                        };
                    if let Some(editor) = artwork.upgrade() {
                        editor.set_busy(false);
                    }
                    match result {
                        Ok(()) => {
                            if let Some(editor) = artwork.upgrade() {
                                editor.saved();
                            }
                            if let Some(dialog) = dialog.upgrade() {
                                dialog.close();
                            }
                        }
                        Err(error) => {
                            if let Some(editor) = artwork.upgrade() {
                                editor.message(&error, true);
                            }
                        }
                    }
                });
            });
            shell.present_selected_dialog(&edit_dialog);
        });
    }
}

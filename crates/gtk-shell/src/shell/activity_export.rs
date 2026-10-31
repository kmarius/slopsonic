use std::io::Write;
use std::rc::Rc;
use std::sync::Arc;

use gtk::glib;
use gtk::prelude::*;

use crate::shell::Shell;

impl Shell {
    pub(crate) fn export_activity_dialog(
        self: &Rc<Self>,
        format: Option<library::ActivityCsvFormat>,
        source_id: Option<sources::SourceId>,
    ) {
        let shell = Rc::clone(self);
        glib::spawn_future_local(async move {
            let resource = gtk_preferences::ui_resource::BACKUP_DIALOG_RESOURCE;
            let dialog: gtk::FileDialog = gtk_widgets::ui_resource::object(
                &gtk_widgets::ui_resource::builder(resource),
                resource,
                "activity_chooser",
            );
            dialog.set_initial_name(Some(if format.is_some() {
                "activity.csv"
            } else {
                "activity.jsonl"
            }));
            let Ok(file) = dialog.save_future(Some(&shell.chrome.window)).await else {
                return;
            };
            let Some(path) = file.path() else {
                return;
            };
            let database = Arc::clone(&shell.products.library);
            let task = shell.products.runtime.spawn(async move {
                let mut output = std::io::BufWriter::new(std::fs::File::create(path)?);
                let count = match format {
                    Some(format) => {
                        database
                            .export_activity_csv(&mut output, format, source_id.as_ref())
                            .await
                    }
                    None => {
                        database
                            .export_activity_jsonl(&mut output, source_id.as_ref())
                            .await
                    }
                }?;
                output.flush()?;
                Ok::<_, library::LibraryError>(count)
            });
            if let Ok(Err(error)) = task.await {
                shell
                    .chrome
                    .toast_overlay
                    .add_toast(adw::Toast::new(&error.to_string()));
            }
        });
    }
}

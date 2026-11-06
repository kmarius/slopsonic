use std::cell::{Cell, RefCell};
use std::ops::ControlFlow;
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::OnceLock;

use adw::prelude::*;
use app_identity::{APP_ID, DISPLAY_NAME, STABLE_APP_ID};
use gtk::gio;
use tracing::error;

use slopsonic_core::runtime::RuntimeInputs;

const ICON_RESOURCE_ROOT: &str = "/io/github/kmarius/Slopsonic/icons/hicolor";
const GTK_DECORATION_LAYOUT_OPTION: &str = "gtk-decoration-layout";

#[derive(Default)]
struct ApplicationOptions {
    decoration_layout: Option<String>,
}

pub fn run_application<F, Fut>(
    settings: slopsonic_core::settings::Settings,
    bootstrap: F,
) -> ExitCode
where
    F: FnOnce() -> Fut + 'static,
    Fut: std::future::Future<Output = Result<RuntimeInputs, String>> + 'static,
{
    let (app, options) = application();
    connect_startup_configuration(&app, Rc::clone(&options));
    let bootstrap = Rc::new(RefCell::new(Some(bootstrap)));
    let quitting = Rc::new(Cell::new(false));
    app.connect_activate(move |app| {
        if quitting.get() {
            return;
        }
        if let Some(window) = app
            .active_window()
            .or_else(|| app.windows().into_iter().next())
        {
            present_window(&window);
            return;
        }
        let Some(bootstrap) = bootstrap.borrow_mut().take() else {
            return;
        };
        let app = app.clone();
        let hold = app.hold();
        let settings = settings.clone();
        let quitting = Rc::clone(&quitting);
        gtk::glib::spawn_future_local(async move {
            let _hold = hold;
            if let Err(error) =
                crate::shell::build::build(&app, settings, bootstrap(), quitting).await
            {
                error!(%error, "failed to start Slopsonic");
                present_startup_error(&app, &error);
            }
        });
    });

    let code = app.run().into();
    for window in app.windows() {
        window.destroy();
    }
    code
}

pub fn run_startup_error_application(error: String) -> ExitCode {
    let (app, options) = application();
    connect_startup_configuration(&app, Rc::clone(&options));
    app.connect_activate(move |app| {
        if let Some(window) = app.active_window() {
            present_window(&window);
        } else {
            present_startup_error(app, &error);
        }
    });
    let code = app.run().into();
    for window in app.windows() {
        window.destroy();
    }
    code
}

fn application() -> (adw::Application, Rc<RefCell<ApplicationOptions>>) {
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.set_option_context_summary(Some(
        "Open audio files or M3U, PLS and XSPF playlists. Use --headless --help for playback without a window.",
    ));
    app.add_main_option(
        GTK_DECORATION_LAYOUT_OPTION,
        0u8.into(),
        gtk::glib::OptionFlags::NONE,
        gtk::glib::OptionArg::String,
        "Override GTK's window control layout for this run",
        Some("LAYOUT"),
    );
    let options = Rc::new(RefCell::new(ApplicationOptions::default()));
    let requested_options = Rc::clone(&options);
    app.connect_handle_local_options(move |_, options| {
        if let Ok(Some(layout)) = options.lookup::<String>(GTK_DECORATION_LAYOUT_OPTION) {
            requested_options.borrow_mut().decoration_layout = Some(layout);
        }
        ControlFlow::Continue(())
    });

    let quit = gio::SimpleAction::new("quit", None);
    let quit_app = app.clone();
    quit.connect_activate(move |_, _| quit_app.quit());
    app.add_action(&quit);
    app.set_accels_for_action("app.quit", &["<Control>q"]);
    app.set_accels_for_action("window.close", &["<Control>w"]);
    (app, options)
}

fn connect_startup_configuration(app: &adw::Application, options: Rc<RefCell<ApplicationOptions>>) {
    app.connect_startup(move |_| {
        configure_app_icon();
        let requested_options = options.borrow();
        let Some(layout) = requested_options.decoration_layout.as_deref() else {
            return;
        };
        let Some(settings) = gtk::Settings::default() else {
            error!("could not apply the requested GTK window control layout");
            return;
        };
        settings.set_gtk_decoration_layout(Some(layout));
    });
}

pub(crate) fn present_window(window: &impl IsA<gtk::Window>) {
    let window = window.as_ref();
    let had_focus = gtk::prelude::RootExt::focus(window).is_some();
    window.present();
    if !had_focus {
        gtk::prelude::RootExt::set_focus(window, None::<&gtk::Widget>);
    }
}

pub(crate) fn application_window(
    app: &adw::Application,
    title: &str,
    default_width: i32,
    default_height: i32,
    content: &gtk::Box,
) -> gtk::ApplicationWindow {
    adw::ApplicationWindow::builder()
        .application(app)
        .title(title)
        .default_width(default_width)
        .default_height(default_height)
        .content(content)
        .build()
        .upcast()
}

fn present_startup_error(app: &adw::Application, error: &str) {
    let resource = crate::ui_resource::STARTUP_ERROR_RESOURCE;
    let builder = gtk_widgets::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, {
        content: gtk::Box,
        status: adw::StatusPage,
    });
    status.set_icon_name(Some(STABLE_APP_ID));
    status.set_title(DISPLAY_NAME);
    status.set_description(Some(error));
    let window = application_window(app, DISPLAY_NAME, 480, 320, &content);
    present_window(&window);
}

fn configure_app_icon() {
    if let Err(error) = register_resources() {
        error!(%error, "failed to register Slopsonic's interface resources");
    }
    gtk::Window::set_default_icon_name(STABLE_APP_ID);
    let Some(display) = gtk::gdk::Display::default() else {
        return;
    };
    gtk::IconTheme::for_display(&display).add_resource_path(ICON_RESOURCE_ROOT);
}

fn register_resources() -> Result<(), String> {
    gtk_widgets::register_resources()?;
    gtk_editors::register_resources()?;
    gtk_media_menus::register_resources()?;
    gtk_preferences::register_resources()?;
    gtk_player::register_resources()?;
    gtk_library::register_resources()?;
    static REGISTERED: OnceLock<Result<(), String>> = OnceLock::new();
    REGISTERED
        .get_or_init(|| {
            gio::resources_register_include!("slopsonic.gresource")
                .map_err(|error| error.to_string())
        })
        .clone()
}

#[cfg(test)]
fn verify_interface_resources() -> Result<(), String> {
    register_resources()?;
    gtk_widgets::ui_resource::verify_icons()?;
    for path in crate::ui_resource::INTERFACE_RESOURCE_PATHS
        .iter()
        .chain(gtk_widgets::ui_resource::INTERFACE_RESOURCE_PATHS)
        .chain(gtk_editors::ui_resource::INTERFACE_RESOURCE_PATHS)
        .chain(gtk_media_menus::ui_resource::INTERFACE_RESOURCE_PATHS)
        .chain(gtk_preferences::ui_resource::INTERFACE_RESOURCE_PATHS)
        .chain(gtk_player::ui_resource::INTERFACE_RESOURCE_PATHS)
        .chain(gtk_library::ui_resource::INTERFACE_RESOURCE_PATHS)
    {
        gio::resources_lookup_data(path, gio::ResourceLookupFlags::NONE)
            .map_err(|error| format!("missing compiled Slopsonic resource {path}: {error}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composed_interface_resources_are_compiled() {
        verify_interface_resources().expect("compiled interface resources");
    }
}

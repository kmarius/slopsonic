use slopsonic_core::{app, diagnostics, paths};
use std::process::ExitCode;

fn main() -> ExitCode {
    process_memory::configure_allocator();
    if let Some(result) = playback_gstreamer::restart_with_http1() {
        return result;
    }
    let settings = app::startup_settings(&paths::roots());
    let (diagnostics, _stderr_guard) = diagnostics::Diagnostics::install(paths::state_dir());

    let initial_settings = settings.load().ui.clone();
    let bootstrap = move || async move {
        app::runtime_inputs(diagnostics, settings, paths::roots(), || {
            playback_gstreamer::GStreamerPlaybackBackend::new()
                .map(|backend| Box::new(backend) as Box<dyn playback::PlaybackBackend>)
                .map_err(|error| error.to_string())
        })
        .await
    };
    match app::with_runtime(|_| gtk_shell::run_application(initial_settings, bootstrap)) {
        Ok(code) => code,
        Err(error) => gtk_shell::run_startup_error_application(error.to_string()),
    }
}

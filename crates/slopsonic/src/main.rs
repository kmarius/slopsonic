use slopsonic_core::{app, diagnostics, paths};
use std::process::ExitCode;
use tracing::info;

fn main() -> ExitCode {
    process_memory::configure_allocator();
    if let Some(result) = playback_gstreamer::restart_with_http1() {
        return result;
    }
    let settings = app::startup_settings(&paths::roots());
    let (diagnostics, _stderr_guard) = diagnostics::Diagnostics::install(paths::state_dir());
    info!("starting Slopsonic native shell");

    let initial_settings = settings.load().ui.clone();
    let bootstrap = move || async move {
        app::runtime_inputs(diagnostics, settings, paths::roots(), |processing| {
            playback_gstreamer::GStreamerPlaybackBackend::new(processing)
                .map(|backend| Box::new(backend) as Box<dyn playback::PlaybackBackend>)
                .map_err(|error| error.to_string())
        })
        .await
    };
    let runtime = match app::worker_runtime() {
        Ok(runtime) => runtime,
        Err(error) => return gtk_shell::run_startup_error_application(error.to_string()),
    };
    let code = {
        let _guard = runtime.enter();
        gtk_shell::run_application(initial_settings, bootstrap)
    };
    runtime.shutdown_timeout(std::time::Duration::from_secs(1));
    code
}

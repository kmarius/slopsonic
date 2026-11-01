use rufin_core::{app, diagnostics, paths};
use std::env;
use std::io;
use std::process::{Command, ExitCode};
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
        Err(error) => {
            return finish_storage_reset(gtk_shell::run_startup_error_application(
                error.to_string(),
            ));
        }
    };
    let result = {
        let _guard = runtime.enter();
        gtk_shell::run_application(initial_settings, bootstrap)
    };
    if result.storage_reset.is_some() {
        // Wait for file workers too; deleting data while they run can recreate it.
        drop(runtime);
        drop(_stderr_guard);
        return finish_storage_reset(result);
    }
    runtime.shutdown_timeout(std::time::Duration::from_secs(1));
    result.code
}

fn finish_storage_reset(mut result: gtk_shell::ApplicationExit) -> ExitCode {
    loop {
        let Some(reset) = result.storage_reset else {
            return result.code;
        };
        let reset = reset
            .and_then(|request| request.apply(&paths::roots()))
            .and_then(|()| restart_after_storage_reset().map_err(|error| error.to_string()));
        match reset {
            Ok(code) => return code,
            Err(error) => result = gtk_shell::run_startup_error_application(error),
        }
    }
}

fn restart_after_storage_reset() -> io::Result<ExitCode> {
    env::current_exe().and_then(|executable| {
        let mut command = Command::new(executable);
        use std::os::unix::process::CommandExt as _;
        Err(command.exec())
    })
}

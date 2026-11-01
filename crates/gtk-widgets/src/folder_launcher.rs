use desktop_integration::folder_target;

pub async fn open_folder_uri(window: &gtk::ApplicationWindow, uri: &str) -> Result<(), String> {
    let file = folder_target(uri)?;
    gtk::FileLauncher::new(Some(&file))
        .launch_future(Some(window))
        .await
        .map_err(|error| error.to_string())
}

//! Translate source folder URIs at the native file-manager boundary.

use url::Url;

pub fn folder_target(uri: &str) -> Result<gio::File, String> {
    let url = Url::parse(uri).map_err(|error| error.to_string())?;
    Ok(gio::File::for_uri(&file_manager_uri(&url)))
}

fn file_manager_uri(url: &Url) -> String {
    match url.scheme() {
        "http" => format!("dav{}", &url[url::Position::AfterScheme..]),
        "https" => format!("davs{}", &url[url::Position::AfterScheme..]),
        _ => url.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_folders_target_the_file_manager() {
        for (uri, gio_uri) in [
            (
                "https://cloud.test:8443/dav/Artist%20One/",
                "davs://cloud.test:8443/dav/Artist%20One/",
            ),
            ("http://cloud.test/dav/", "dav://cloud.test/dav/"),
            (
                "smb://nas.test/music/Album%23One/",
                "smb://nas.test/music/Album%23One/",
            ),
        ] {
            let url = Url::parse(uri).unwrap();
            assert_eq!(file_manager_uri(&url), gio_uri);
        }
    }
}

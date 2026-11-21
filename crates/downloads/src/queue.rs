use super::*;

pub(super) const QUEUE_VERSION: u32 = 3;
pub(super) const QUEUE_FILE: &str = "queue.json";
pub(super) const QUEUE_PART_FILE: &str = "queue.json.part";

#[derive(Debug, Deserialize, Serialize)]
pub(super) struct QueueFile<T = Vec<DownloadJob>> {
    pub(super) version: u32,
    pub(super) jobs: T,
}

pub(super) async fn persist_queue(root: &Path, jobs: &[DownloadJob]) -> Result<(), String> {
    let directory = source_directory(root);
    tokio::fs::create_dir_all(&directory)
        .await
        .map_err(|error| format!("could not create the download queue directory: {error}"))?;
    let path = directory.join(QUEUE_FILE);
    let part = directory.join(QUEUE_PART_FILE);
    if jobs.is_empty() {
        remove_file_if_present(&part).await?;
        remove_file_if_present(&path).await?;
        return Ok(());
    }
    let encoded = serde_json::to_vec(&QueueFile {
        version: QUEUE_VERSION,
        jobs,
    })
    .map_err(|error| format!("could not encode the download queue: {error}"))?;
    tokio::fs::write(&part, encoded)
        .await
        .map_err(|error| format!("could not save the download queue: {error}"))?;
    tokio::fs::rename(&part, &path)
        .await
        .map_err(|error| format!("could not finish the download queue: {error}"))
}

pub(super) fn read_queue_file(
    directory: &Path,
) -> Result<Option<(Vec<u8>, bool, PathBuf, PathBuf)>, String> {
    let path = directory.join(QUEUE_FILE);
    let part = directory.join(QUEUE_PART_FILE);
    match std::fs::read(&path) {
        Ok(bytes) => Ok(Some((bytes, false, path, part))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => match std::fs::read(&part) {
            Ok(bytes) => Ok(Some((bytes, true, path, part))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(format!("could not read {}: {error}", part.display())),
        },
        Err(error) => Err(format!("could not read {}: {error}", path.display())),
    }
}

pub(super) async fn load_queue(root: &Path) -> Result<Vec<DownloadJob>, String> {
    let Some((bytes, recovered, path, part)) = read_queue_file(&source_directory(root))? else {
        return Ok(Vec::new());
    };
    let queue = serde_json::from_slice::<QueueFile>(&bytes)
        .map_err(|error| format!("could not decode {}: {error}", path.display()))?;
    if queue.version != QUEUE_VERSION {
        return Err("the saved download queue has an unsupported version".to_string());
    }
    if recovered && let Err(error) = std::fs::rename(&part, &path) {
        warn!(%error, path = %part.display(), "could not finish recovering the download queue");
    }
    Ok(queue.jobs)
}

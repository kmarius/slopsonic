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
    write_queue(&source_directory(root), jobs).await
}

async fn write_queue(directory: &Path, jobs: &[DownloadJob]) -> Result<(), String> {
    tokio::fs::create_dir_all(directory)
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

fn decode_queue(directory: &Path) -> Result<Option<Vec<DownloadJob>>, String> {
    let Some((bytes, recovered, path, part)) = read_queue_file(directory)? else {
        return Ok(None);
    };
    let queue = serde_json::from_slice::<QueueFile>(&bytes)
        .map_err(|error| format!("could not decode {}: {error}", path.display()))?;
    if queue.version != QUEUE_VERSION {
        return Err("the saved download queue has an unsupported version".to_string());
    }
    if recovered && let Err(error) = std::fs::rename(&part, &path) {
        warn!(%error, path = %part.display(), "could not finish recovering the download queue");
    }
    Ok(Some(queue.jobs))
}

pub(super) async fn load_queue(root: &Path) -> Result<Vec<DownloadJob>, String> {
    Ok(decode_queue(&source_directory(root))?.unwrap_or_default())
}

/// Moves the queues of former per-server and direct folders into the server queue, rewriting
/// their media URIs; media that belongs to no server is dropped.
pub(super) async fn merge_legacy_queues(root: &Path, database: &Database) -> Result<(), String> {
    let mut saved = load_queue(root).await?;
    let mut entries = match tokio::fs::read_dir(root).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    let mut merged = Vec::new();
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|error| error.to_string())?
    {
        let directory = entry.path();
        if entry.file_name() == SERVER_DIRECTORY || !directory.is_dir() {
            continue;
        }
        let jobs = match decode_queue(&directory) {
            Ok(Some(jobs)) => jobs,
            Ok(None) => continue,
            Err(error) => {
                warn!(%error, path = %directory.display(), "skipped a former download queue");
                continue;
            }
        };
        for mut job in jobs {
            let mut known = HashSet::new();
            let completed = std::mem::take(&mut job.completed);
            let remaining = std::mem::take(&mut job.remaining);
            for (done, uri) in completed
                .into_iter()
                .map(|uri| (true, uri))
                .chain(remaining.into_iter().map(|uri| (false, uri)))
            {
                let uri = library::single_server_uri(&uri);
                if !known.insert(uri.clone()) || !is_server_media(database, &uri).await {
                    continue;
                }
                if done {
                    job.completed.push(uri);
                } else {
                    job.remaining.push_back(uri);
                }
            }
            if !job.remaining.is_empty() && saved.iter().all(|existing| existing.id != job.id) {
                saved.push(job);
            }
        }
        merged.push(directory);
    }
    if merged.is_empty() {
        return Ok(());
    }
    // The merged queue is saved before the former files go, so a failure loses nothing.
    persist_queue(root, &saved).await?;
    for directory in merged {
        write_queue(&directory, &[]).await?;
        let _ = tokio::fs::remove_dir(&directory).await;
    }
    Ok(())
}

/// Empties the former per-server folders after their queues merged: partial downloads there no
/// longer resume, and files no download record uses belong to dropped servers. Files a record
/// still uses, and queue files that could not be merged, stay. Unused files are only judged when
/// the database records downloads under this root as spelled now; a temporary database or a moved
/// root keeps them.
pub(super) async fn sweep_former_folders(root: &Path, database: &Database) {
    let records_trusted = matches!(database.downloads_recorded_under(root).await, Ok(true));
    for folder in former_server_folders(root).await {
        let Ok(mut entries) = tokio::fs::read_dir(&folder).await else {
            continue;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            let name = entry.file_name();
            if !path.is_file() || name == QUEUE_FILE || name == QUEUE_PART_FILE {
                continue;
            }
            // A failed lookup keeps the file.
            let unused = managed_staging_file(&path)
                || records_trusted
                    && matches!(database.download_path_in_use(&path).await, Ok(false));
            if unused && let Err(error) = remove_file_if_present(&path).await {
                warn!(%error, path = %path.display(), "could not remove a former download file");
            }
        }
        let _ = tokio::fs::remove_dir(&folder).await;
    }
}

pub(super) async fn is_server_media(database: &Database, media_uri: &str) -> bool {
    library::entity_parts(media_uri).is_some()
        || database
            .is_server_track(media_uri, &library::ReadCancellation::new())
            .await
            .unwrap_or(false)
}

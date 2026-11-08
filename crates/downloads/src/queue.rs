use super::*;

pub(super) const QUEUE_VERSION: u32 = 3;
pub(super) const QUEUE_FILE: &str = "queue.json";
pub(super) const QUEUE_PART_FILE: &str = "queue.json.part";

#[derive(Debug, Deserialize, Serialize)]
pub(super) struct QueueFile<T = Vec<DownloadJob>> {
    pub(super) version: u32,
    pub(super) jobs: T,
}

pub(super) async fn persist_queue(
    root: &Path,
    source_id: Option<&SourceId>,
    jobs: &[DownloadJob],
) -> Result<(), String> {
    let directory = source_directory(root, source_id);
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
    root: &Path,
    source_id: Option<&SourceId>,
) -> Result<Option<(Vec<u8>, bool, PathBuf, PathBuf)>, String> {
    let directory = source_directory(root, source_id);
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

pub(super) async fn load_queue(
    root: &Path,
    source_id: &SourceId,
) -> Result<Vec<DownloadJob>, String> {
    let Some((bytes, recovered, path, part)) = read_queue_file(root, Some(source_id))? else {
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

pub(super) async fn load_direct_queue(
    root: &Path,
    database: &Database,
) -> Result<Vec<DownloadJob>, String> {
    let Some((bytes, recovered, path, part)) = read_queue_file(root, None)? else {
        return Ok(Vec::new());
    };
    let queue: QueueFile = serde_json::from_slice(&bytes)
        .map_err(|error| format!("could not decode {}: {error}", path.display()))?;
    if queue.version != QUEUE_VERSION {
        return Err("the saved direct download queue has an unsupported version".to_string());
    }
    if recovered && let Err(error) = std::fs::rename(&part, &path) {
        warn!(%error, path=%part.display(), "could not finish recovering the direct download queue");
    }
    let mut grouped = HashMap::<Option<SourceId>, Vec<DownloadJob>>::new();
    for mut job in queue.jobs {
        let completed = std::mem::take(&mut job.completed);
        let remaining = std::mem::take(&mut job.remaining);
        let mut split = HashMap::<Option<SourceId>, DownloadJob>::new();
        for (completed, uri) in completed
            .into_iter()
            .map(|uri| (true, uri))
            .chain(remaining.into_iter().map(|uri| (false, uri)))
        {
            let source = media_source_id(database, &uri).await;
            let part = split.entry(source).or_insert_with(|| job.clone());
            if completed {
                part.completed.push(uri);
            } else {
                part.remaining.push_back(uri);
            }
        }
        for (source, job) in split {
            if !job.remaining.is_empty() {
                grouped.entry(source).or_default().push(job);
            }
        }
    }
    let direct = grouped.remove(&None).unwrap_or_default();
    for (source, jobs) in grouped {
        let source = source.unwrap();
        let mut saved = load_queue(root, &source).await?;
        for job in jobs {
            if let Some(existing) = saved.iter_mut().find(|existing| existing.id == job.id) {
                let mut completed = existing.completed.iter().cloned().collect::<HashSet<_>>();
                let mut remaining = existing.remaining.iter().cloned().collect::<HashSet<_>>();
                existing.completed.extend(
                    job.completed
                        .into_iter()
                        .filter(|uri| completed.insert(uri.clone())),
                );
                existing.remaining.extend(
                    job.remaining
                        .into_iter()
                        .filter(|uri| !completed.contains(uri) && remaining.insert(uri.clone())),
                );
                existing.remaining.retain(|uri| !completed.contains(uri));
            } else {
                saved.push(job);
            }
        }
        persist_queue(root, Some(&source), &saved).await?;
    }
    persist_queue(root, None, &direct).await?;
    let queued = direct
        .iter()
        .flat_map(|job| job.remaining.iter().cloned())
        .collect();
    cleanup_staging(root, None, None, &queued)
        .await
        .map_err(|error| error.to_string())?;
    Ok(direct)
}

pub(super) async fn media_source_id(database: &Database, media_uri: &str) -> Option<SourceId> {
    if let Some((source, _, _)) = library::source_entity_parts(media_uri) {
        return Some(source);
    }
    database
        .track_source_by_uri(media_uri, &library::ReadCancellation::new())
        .await
        .ok()
        .flatten()
}

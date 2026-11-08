use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use audio_processing::ResolvedStream;
use library::Database;
use reqwest::header::{
    ACCEPT_ENCODING, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_RANGE, DATE, ETAG, IF_RANGE,
    LAST_MODIFIED, RANGE,
};
use serde::{Deserialize, Serialize};
use sources::DownloadQuality;
use sources::{SourceError, SourceId, SourceResult};
use tokio::io::AsyncWriteExt;
use tokio::sync::oneshot;

use crate::storage::DownloadAudio;
use crate::{DownloadDirectory, DownloadOwner};

const PARSER_VERSION: i64 = 5;
pub(super) const AUDIO_EXTENSION: &str = "audio";
pub(super) const PART_EXTENSION: &str = "part";
const CHECKPOINT_EXTENSION: &str = "resume";
pub(super) const CUSTOM_STAGING_DIRECTORY: &str = ".slopsonic-partials";

#[derive(Clone)]
pub(super) struct DownloadPaths {
    pub(super) directory: PathBuf,
    pub(super) audio: DownloadAudio,
    pub(super) audio_part: PathBuf,
    pub(super) checkpoint: PathBuf,
}

pub(super) struct FinishedDownload {
    pub paths: DownloadPaths,
    pub completed_size: u64,
}

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
struct TransferCheckpoint {
    representation: String,
    validator: String,
    length: u64,
}

fn resume_validator(headers: &reqwest::header::HeaderMap) -> Option<String> {
    if let Some(etag) = headers.get(ETAG) {
        let etag = etag.to_str().ok()?;
        return strong_etag(etag).then(|| etag.to_string());
    }
    let modified = headers.get(LAST_MODIFIED)?.to_str().ok()?;
    let date = headers.get(DATE)?.to_str().ok()?;
    let age = httpdate::parse_http_date(date)
        .ok()?
        .duration_since(httpdate::parse_http_date(modified).ok()?)
        .ok()?;
    (age >= Duration::from_secs(60)).then(|| modified.to_string())
}

#[derive(Default)]
pub(super) struct TransferClients {
    strict: tokio::sync::OnceCell<reqwest::Client>,
    insecure: tokio::sync::OnceCell<reqwest::Client>,
}

impl TransferClients {
    async fn download_cancellable(
        &self,
        stream: &ResolvedStream,
        representation: &str,
        paths: &DownloadPaths,
        cancellation: &mut oneshot::Receiver<()>,
    ) -> SourceResult<()> {
        let clients = if stream.trust_invalid_certificate() {
            &self.insecure
        } else {
            &self.strict
        };
        let trust_invalid_certificate = stream.trust_invalid_certificate();
        let client = clients
            .get_or_try_init(|| async move {
                reqwest::Client::builder()
                    .referer(false)
                    .danger_accept_invalid_certs(trust_invalid_certificate)
                    .connect_timeout(Duration::from_secs(15))
                    .read_timeout(Duration::from_secs(60))
                    .build()
                    .map_err(download_request_error)
            })
            .await?;
        let resume = read_checkpoint(paths, Some(representation)).await?;
        let response = send_request(client, stream.uri(), resume.as_ref(), cancellation).await?;
        let status = response.status();
        if status == reqwest::StatusCode::OK {
            return download_full(response, paths, representation, cancellation).await;
        }
        if status == reqwest::StatusCode::PARTIAL_CONTENT
            && let Some((checkpoint, offset)) = resume.as_ref()
            && valid_partial(&response, checkpoint, *offset)
        {
            return download_partial(response, paths, checkpoint, *offset, cancellation).await;
        }
        if status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE
            && let Some((checkpoint, offset)) = resume.as_ref()
            && *offset == checkpoint.length
            && unsatisfied_total(&response) == Some(checkpoint.length)
        {
            return Ok(());
        }
        if status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE && resume.is_some() {
            discard_staging(paths).await?;
            let response = send_request(client, stream.uri(), None, cancellation).await?;
            if response.status() == reqwest::StatusCode::OK {
                return download_full(response, paths, representation, cancellation).await;
            }
            if !response.status().is_success() {
                return response_error(response.status(), paths).await;
            }
            return Err(SourceError::Other(
                "the download server returned an unsupported successful response".to_string(),
            ));
        }
        if !status.is_success() {
            return response_error(status, paths).await;
        }
        Err(SourceError::Other(
            "the download server returned an unsupported successful response".to_string(),
        ))
    }
}

fn representation_key(
    source_id: Option<&SourceId>,
    media_uri: &str,
    quality: DownloadQuality,
    redacted_uri: &str,
) -> String {
    #[derive(Serialize)]
    struct StoredDownload<'a> {
        media_uri: &'a str,
        quality: DownloadQuality,
    }
    let value = serde_json::to_vec(&(
        source_id,
        StoredDownload { media_uri, quality },
        redacted_uri,
    ))
    .expect("a download representation can be encoded");
    hash_id_bytes(&value)
}

async fn send_request(
    client: &reqwest::Client,
    uri: &str,
    resume: Option<&(TransferCheckpoint, u64)>,
    cancellation: &mut oneshot::Receiver<()>,
) -> SourceResult<reqwest::Response> {
    let mut request = client.get(uri).header(ACCEPT_ENCODING, "identity");
    if let Some((checkpoint, offset)) = resume {
        request = request
            .header(RANGE, format!("bytes={offset}-"))
            .header(IF_RANGE, &checkpoint.validator);
    }
    tokio::select! {
        biased;
        result = request.send() => result.map_err(download_request_error),
        _ = &mut *cancellation => Err(SourceError::Cancelled),
    }
}

async fn download_full(
    response: reqwest::Response,
    paths: &DownloadPaths,
    representation: &str,
    cancellation: &mut oneshot::Receiver<()>,
) -> SourceResult<()> {
    if !identity_response(&response) {
        discard_staging(paths).await?;
        return Err(SourceError::Other(
            "the download response used an unsupported encoding".to_string(),
        ));
    }
    let expected = response_length(&response).filter(|length| *length > 0);
    let checkpoint = expected.and_then(|length| {
        resume_validator(response.headers()).map(|validator| TransferCheckpoint {
            representation: representation.to_string(),
            validator,
            length,
        })
    });
    discard_staging(paths).await?;
    let file = tokio::fs::File::create(&paths.audio_part)
        .await
        .map_err(|error| SourceError::Other(format!("could not create download: {error}")))?;
    if let Some(checkpoint) = checkpoint.as_ref() {
        write_checkpoint(paths, checkpoint).await?;
    }
    write_response(
        response,
        paths,
        file,
        expected,
        checkpoint.is_some(),
        0,
        cancellation,
    )
    .await?;
    Ok(())
}

async fn download_partial(
    response: reqwest::Response,
    paths: &DownloadPaths,
    checkpoint: &TransferCheckpoint,
    offset: u64,
    cancellation: &mut oneshot::Receiver<()>,
) -> SourceResult<()> {
    let file = tokio::fs::OpenOptions::new()
        .append(true)
        .open(&paths.audio_part)
        .await
        .map_err(|error| SourceError::Other(format!("could not resume download: {error}")))?;
    write_response(
        response,
        paths,
        file,
        Some(checkpoint.length - offset),
        true,
        offset,
        cancellation,
    )
    .await?;
    Ok(())
}

async fn write_response(
    response: reqwest::Response,
    paths: &DownloadPaths,
    mut file: tokio::fs::File,
    expected: Option<u64>,
    resumable: bool,
    starting_length: u64,
    cancellation: &mut oneshot::Receiver<()>,
) -> SourceResult<()> {
    let written = match stream_body(response, &mut file, expected, cancellation).await {
        Ok(written) => written,
        Err(error) => return finish_failed_body(paths, file, resumable, error).await,
    };
    if written == 0 || expected.is_some_and(|expected| written != expected) {
        let error = if expected.is_some() {
            SourceError::Network("the download ended before its declared length".to_string())
        } else {
            SourceError::Other("the download response was empty".to_string())
        };
        return finish_failed_body(
            paths,
            file,
            resumable && (starting_length > 0 || written > 0),
            error,
        )
        .await;
    }
    finish_file(file).await
}

async fn stream_body(
    mut response: reqwest::Response,
    file: &mut tokio::fs::File,
    maximum: Option<u64>,
    cancellation: &mut oneshot::Receiver<()>,
) -> SourceResult<u64> {
    let mut bytes_written = 0u64;
    loop {
        let chunk = tokio::select! {
            biased;
            result = tokio::time::timeout(Duration::from_secs(60), response.chunk()) => result
                .map_err(|_| SourceError::Network("the download stalled".to_string()))?
                .map_err(download_request_error)?,
            _ = &mut *cancellation => return Err(SourceError::Cancelled),
        };
        let Some(chunk) = chunk else {
            break;
        };
        let next = bytes_written.saturating_add(chunk.len() as u64);
        if maximum.is_some_and(|maximum| next > maximum) {
            return Err(SourceError::Other(
                "the download response exceeded its declared length".to_string(),
            ));
        }
        file.write_all(&chunk)
            .await
            .map_err(|error| SourceError::Other(format!("could not write download: {error}")))?;
        bytes_written = next;
    }
    Ok(bytes_written)
}

async fn finish_failed_body<T>(
    paths: &DownloadPaths,
    mut file: tokio::fs::File,
    resumable: bool,
    error: SourceError,
) -> SourceResult<T> {
    if resumable && file.flush().await.is_ok() && file.sync_all().await.is_ok() {
        return Err(error);
    }
    drop(file);
    discard_staging(paths).await?;
    Err(error)
}

async fn finish_file(mut file: tokio::fs::File) -> SourceResult<()> {
    file.flush()
        .await
        .map_err(|error| SourceError::Other(format!("could not finish download: {error}")))?;
    file.sync_all()
        .await
        .map_err(|error| SourceError::Other(format!("could not save download: {error}")))
}

async fn read_checkpoint(
    paths: &DownloadPaths,
    representation: Option<&str>,
) -> SourceResult<Option<(TransferCheckpoint, u64)>> {
    let encoded = match tokio::fs::read(&paths.checkpoint).await {
        Ok(encoded) => encoded,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            discard_staging(paths).await?;
            return Ok(None);
        }
        Err(error) => {
            return Err(SourceError::Other(format!(
                "could not read download checkpoint: {error}"
            )));
        }
    };
    let checkpoint = serde_json::from_slice::<TransferCheckpoint>(&encoded).ok();
    let length = tokio::fs::metadata(&paths.audio_part)
        .await
        .ok()
        .map(|metadata| metadata.len());
    if checkpoint.as_ref().is_some_and(|checkpoint| {
        !checkpoint.representation.is_empty()
            && representation.is_none_or(|expected| checkpoint.representation == expected)
            && checkpoint.length > 0
            && (strong_etag(&checkpoint.validator)
                || httpdate::parse_http_date(&checkpoint.validator).is_ok())
    }) && let (Some(checkpoint), Some(length)) = (checkpoint, length)
        && length > 0
        && length <= checkpoint.length
    {
        return Ok(Some((checkpoint, length)));
    }
    discard_staging(paths).await?;
    Ok(None)
}

async fn write_checkpoint(
    paths: &DownloadPaths,
    checkpoint: &TransferCheckpoint,
) -> SourceResult<()> {
    let encoded = serde_json::to_vec(checkpoint)
        .map_err(|error| SourceError::Other(format!("could not encode checkpoint: {error}")))?;
    tokio::fs::write(&paths.checkpoint, encoded)
        .await
        .map_err(|error| SourceError::Other(format!("could not write checkpoint: {error}")))
}

pub(super) async fn discard_staging(paths: &DownloadPaths) -> SourceResult<()> {
    for path in [&paths.audio_part, &paths.checkpoint] {
        remove_file_if_present(path)
            .await
            .map_err(SourceError::Other)?;
    }
    Ok(())
}

pub(super) async fn cleanup_staging(
    root: &Path,
    source_id: Option<&SourceId>,
    directory: Option<&DownloadDirectory>,
    media: &HashSet<String>,
) -> SourceResult<()> {
    let expected = media
        .iter()
        .map(|identity| staging_paths(root, source_id, identity, directory))
        .flat_map(|paths| [paths.audio_part.clone(), paths.checkpoint.clone()])
        .collect::<HashSet<_>>();
    let custom = directory
        .and_then(DownloadDirectory::native)
        .map(|directory| {
            directory
                .join(CUSTOM_STAGING_DIRECTORY)
                .join(hash_id(source_id.map_or("direct", SourceId::as_str)))
        });
    for directory in [Some(source_directory(root, source_id)), custom.clone()]
        .into_iter()
        .flatten()
    {
        let mut entries = match tokio::fs::read_dir(&directory).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(SourceError::Other(format!(
                    "could not inspect download staging at {}: {error}",
                    directory.display()
                )));
            }
        };
        while let Some(entry) = entries.next_entry().await.map_err(|error| {
            SourceError::Other(format!("could not inspect download staging: {error}"))
        })? {
            let path = entry.path();
            if managed_staging_file(&path) && !expected.contains(&path) {
                remove_file_if_present(&path)
                    .await
                    .map_err(SourceError::Other)?;
            }
        }
    }
    if let Some(source_staging) = custom {
        let _ = tokio::fs::remove_dir(source_staging).await;
    }
    if let Some(directory) = directory.and_then(DownloadDirectory::native) {
        let _ = tokio::fs::remove_dir(directory.join(CUSTOM_STAGING_DIRECTORY)).await;
    }
    Ok(())
}

fn managed_staging_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(".audio.part") || name.ends_with(".audio.part.resume"))
}

fn response_length(response: &reqwest::Response) -> Option<u64> {
    response
        .headers()
        .get(CONTENT_LENGTH)?
        .to_str()
        .ok()?
        .parse()
        .ok()
}

fn strong_etag(value: &str) -> bool {
    !value.starts_with("W/") && value.starts_with('"') && value.ends_with('"') && value.len() >= 2
}

fn valid_partial(
    response: &reqwest::Response,
    checkpoint: &TransferCheckpoint,
    offset: u64,
) -> bool {
    if !identity_response(response) {
        return false;
    }
    let Some((start, end, total)) = satisfied_range(response) else {
        return false;
    };
    let expected_suffix = checkpoint.length.saturating_sub(offset);
    start == offset
        && end.checked_add(1) == Some(total)
        && total == checkpoint.length
        && response_length(response).is_none_or(|length| length == expected_suffix)
}

fn identity_response(response: &reqwest::Response) -> bool {
    response
        .headers()
        .get(CONTENT_ENCODING)
        .and_then(|value| value.to_str().ok())
        .is_none_or(|value| value.eq_ignore_ascii_case("identity"))
}

fn satisfied_range(response: &reqwest::Response) -> Option<(u64, u64, u64)> {
    let value = response.headers().get(CONTENT_RANGE)?.to_str().ok()?;
    let value = value.strip_prefix("bytes ")?;
    let (range, total) = value.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    Some((start.parse().ok()?, end.parse().ok()?, total.parse().ok()?))
}

fn unsatisfied_total(response: &reqwest::Response) -> Option<u64> {
    response
        .headers()
        .get(CONTENT_RANGE)?
        .to_str()
        .ok()?
        .strip_prefix("bytes */")?
        .parse()
        .ok()
}

fn status_error(status: u16) -> SourceError {
    match status {
        401 | 403 => SourceError::Auth("the download was not authorized".to_string()),
        404 => SourceError::NotFound,
        status => SourceError::Server {
            status,
            message: "the download request failed".to_string(),
        },
    }
}

async fn response_error<T>(status: reqwest::StatusCode, paths: &DownloadPaths) -> SourceResult<T> {
    if status == reqwest::StatusCode::NOT_FOUND
        || (status.is_client_error() && !matches!(status.as_u16(), 401 | 403 | 429))
    {
        discard_staging(paths).await?;
    }
    Err(status_error(status.as_u16()))
}

fn download_request_error(error: reqwest::Error) -> SourceError {
    if error.is_timeout() {
        SourceError::Network("the download timed out".to_string())
    } else if error.is_connect() {
        SourceError::Network("could not connect for the download".to_string())
    } else if error
        .to_string()
        .to_ascii_lowercase()
        .contains("certificate")
    {
        SourceError::Tls("the download certificate was rejected".to_string())
    } else {
        SourceError::Network("the download was interrupted".to_string())
    }
}

pub(super) async fn prepare_download_directories(paths: &DownloadPaths) -> SourceResult<()> {
    for directory in [
        Some(paths.directory.as_path()),
        paths.audio.native().and_then(Path::parent),
        paths.audio_part.parent(),
    ]
    .into_iter()
    .flatten()
    {
        tokio::fs::create_dir_all(directory)
            .await
            .map_err(|error| {
                SourceError::Other(format!("could not create a download directory: {error}"))
            })?;
    }
    Ok(())
}

pub(super) async fn run_transfer(
    source_id: Option<&SourceId>,
    media_uri: &str,
    quality: DownloadQuality,
    stream: &ResolvedStream,
    paths: &DownloadPaths,
    transfers: &TransferClients,
    cancellation: &mut oneshot::Receiver<()>,
) -> SourceResult<()> {
    prepare_download_directories(paths).await?;
    let representation = representation_key(source_id, media_uri, quality, stream.redacted_uri());
    if let Some(path) = library::file_media_path(stream.uri()) {
        tokio::select! {
            result = tokio::fs::copy(path, &paths.audio_part) => result.map(|_| ()).map_err(|error| SourceError::Other(error.to_string())),
            _ = &mut *cancellation => Err(SourceError::Cancelled),
        }
    } else {
        transfers
            .download_cancellable(stream, &representation, paths, cancellation)
            .await
    }
}

pub(super) async fn encode_download(
    stream: ResolvedStream,
    encoding: library::AudioEncoding,
    paths: &DownloadPaths,
    cancellation: &mut oneshot::Receiver<()>,
) -> SourceResult<()> {
    prepare_download_directories(paths).await?;
    discard_staging(paths).await?;
    let output = paths.audio_part.clone();
    let cancelled = tokio_util::sync::CancellationToken::new();
    let stop = cancelled.clone();
    let mut task = tokio::task::spawn_blocking(move || {
        audio_processing::transcode_audio(&stream, encoding, &output, || stop.is_cancelled())
            .map_err(|error| match error.kind() {
                std::io::ErrorKind::Interrupted => SourceError::Cancelled,
                std::io::ErrorKind::ConnectionAborted => SourceError::Network(error.to_string()),
                std::io::ErrorKind::PermissionDenied => SourceError::Auth(error.to_string()),
                std::io::ErrorKind::NotFound => SourceError::NotFound,
                _ => SourceError::Other(error.to_string()),
            })
    });
    let result = tokio::select! {
        result = &mut task => result.map_err(|error| SourceError::Other(error.to_string()))?,
        _ = &mut *cancellation => { cancelled.cancel(); let _ = task.await; Err(SourceError::Cancelled) },
    };
    if result.is_err() {
        discard_staging(paths).await?;
    }
    result
}

pub(super) fn download_owners(
    file: &library::DownloadFile,
) -> Result<HashSet<DownloadOwner>, String> {
    let mut owners: HashSet<DownloadOwner> = serde_json::from_str(if file.owners.is_empty() {
        "[]"
    } else {
        &file.owners
    })
    .map_err(|error| error.to_string())?;
    if owners.is_empty() {
        owners.insert(DownloadOwner::Retained);
    }
    Ok(owners)
}

pub(super) fn completed_download_paths(
    root: &Path,
    file: &library::DownloadFile,
) -> Result<DownloadPaths, String> {
    let source = file.source_id.as_deref().map(SourceId::new);
    let mut paths = download_paths(root, source.as_ref(), &file.media_uri);
    let path =
        library::file_media_path(&file.path).ok_or("The completed download has no local path")?;
    let root = if file.storage_root.is_empty() {
        path.parent().map(Path::to_owned)
    } else {
        Some(PathBuf::from(&file.storage_root))
    };
    paths.audio = DownloadAudio::Native { root, path };
    Ok(paths)
}

pub(super) async fn finalize_download(paths: &DownloadPaths) -> Result<FinishedDownload, String> {
    let completed_size = tokio::fs::metadata(&paths.audio_part)
        .await
        .map_err(|error| error.to_string())?
        .len();
    let mut paths = paths.clone();
    paths.audio = paths.audio.publish(&paths.audio_part).await?;
    let _ = remove_file_if_present(&paths.audio_part).await;
    let _ = remove_file_if_present(&paths.checkpoint).await;
    Ok(FinishedDownload {
        paths,
        completed_size,
    })
}

pub(super) async fn download_access(
    database: &Database,
    media_uri: &str,
    paths: &DownloadPaths,
    completed_size: Option<i64>,
) -> Result<library::DownloadAccessWrite, String> {
    let loudness_analysis_key = database
        .audio_analysis_key(media_uri)
        .await
        .map_err(|error| error.to_string())?;
    let metadata = paths
        .audio
        .inspect()
        .await?
        .ok_or("The downloaded file is unavailable")?;
    let (root, relative_path) = local_access_projection(paths)?;
    Ok(library::DownloadAccessWrite {
        media_uri: media_uri.into(),
        path: paths.audio.location()?,
        root,
        relative_path,
        size_bytes: metadata
            .size
            .and_then(|size| i64::try_from(size).ok())
            .or(completed_size)
            .ok_or("The download has no completed size")?,
        mtime_ns: metadata.mtime_ns,
        device_id: None,
        inode: None,
        parser_version: PARSER_VERSION,
        access_uri: paths.audio.access_uri()?,
        loudness_analysis_key,
    })
}

pub(super) async fn remove_download_files(paths: &DownloadPaths) -> Result<bool, String> {
    let mut present = paths.audio.remove().await?;
    for path in [&paths.audio_part, &paths.checkpoint] {
        match tokio::fs::remove_file(path).await {
            Ok(()) => present = true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("could not remove {}: {error}", path.display())),
        }
    }
    if let DownloadAudio::Native {
        root: Some(root),
        path,
    } = &paths.audio
        && let Some(album) = path.parent()
        && let Some(artist) = album.parent()
    {
        for directory in [album, artist] {
            if directory == root || tokio::fs::remove_dir(directory).await.is_err() {
                break;
            }
        }
    }
    Ok(present)
}

pub(super) async fn remove_file_if_present(path: &Path) -> Result<(), String> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("could not replace {}: {error}", path.display())),
    }
}

pub(super) fn source_directory(root: &Path, source_id: Option<&SourceId>) -> PathBuf {
    root.join(source_id.map_or_else(
        || "direct".to_string(),
        |source_id| hash_id(source_id.as_str()),
    ))
}

pub(super) fn download_paths(
    root: &Path,
    source_id: Option<&SourceId>,
    identity: &str,
) -> DownloadPaths {
    let directory = source_directory(root, source_id);
    let stem = hash_id(identity);
    let audio = directory.join(format!("{stem}.{AUDIO_EXTENSION}"));
    let audio_part = part_path(&audio);
    let checkpoint = checkpoint_path(&audio_part);
    DownloadPaths {
        audio: DownloadAudio::Native {
            root: None,
            path: audio,
        },
        audio_part,
        checkpoint,
        directory,
    }
}

pub(super) fn staging_paths(
    root: &Path,
    source_id: Option<&SourceId>,
    identity: &str,
    directory: Option<&DownloadDirectory>,
) -> DownloadPaths {
    let mut paths = download_paths(root, source_id, identity);
    let Some(directory) = directory.and_then(DownloadDirectory::native) else {
        return paths;
    };
    let staging = directory
        .join(CUSTOM_STAGING_DIRECTORY)
        .join(hash_id(source_id.map_or("direct", SourceId::as_str)));
    paths.audio_part = staging.join(format!(
        "{}.{}.{}",
        hash_id(identity),
        AUDIO_EXTENSION,
        PART_EXTENSION
    ));
    paths.checkpoint = checkpoint_path(&paths.audio_part);
    paths
}

pub(super) fn new_download_paths(
    root: &Path,
    source_id: Option<&SourceId>,
    media_uri: &str,
    media: Option<&library::DownloadMetadata>,
    directory: Option<&DownloadDirectory>,
    transcoded_extension: Option<&str>,
) -> DownloadPaths {
    let mut paths = staging_paths(root, source_id, media_uri, directory);
    let audio_root = directory
        .and_then(DownloadDirectory::native)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| source_directory(root, source_id));
    let artist = safe_path_component(media.map_or("", |media| &media.artist), "Unknown Artist");
    let album = safe_path_component(media.map_or("", |media| &media.album), "Unknown Album");
    let title = safe_path_component(media.map_or("", |media| &media.title), "Untitled");
    let id = hash_id(media_uri);
    let short_id = id.chars().take(12).collect::<String>();
    let extension = download_extension(
        media.and_then(|media| media.source_format.as_deref()),
        transcoded_extension,
    );
    let file_name = format!(
        "{:02}-{:02} {title} [{}].{extension}",
        media.map(|media| media.disc_number).unwrap_or_default(),
        media.map(|media| media.track_number).unwrap_or_default(),
        short_id
    );
    let audio = audio_root.join(&artist).join(&album).join(&file_name);
    paths.audio = DownloadAudio::Native {
        root: Some(audio_root),
        path: audio,
    };
    paths
}

pub(super) fn local_access_projection(paths: &DownloadPaths) -> Result<(String, String), String> {
    let (root, relative) = paths.audio.projection(&paths.directory)?;
    if !normal_relative_path(Path::new(&relative)) {
        return Err("The downloaded track has an invalid relative storage path".into());
    }
    Ok((root, relative))
}

fn part_path(path: &Path) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(format!(".{PART_EXTENSION}"));
    value.into()
}

fn checkpoint_path(audio_part: &Path) -> PathBuf {
    let mut value = audio_part.as_os_str().to_os_string();
    value.push(format!(".{CHECKPOINT_EXTENSION}"));
    value.into()
}

fn normal_relative_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
}

fn safe_path_component(value: &str, fallback: &str) -> String {
    let sanitized = value
        .trim()
        .chars()
        .map(|character| {
            if character.is_control()
                || matches!(
                    character,
                    '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
                )
            {
                '_'
            } else {
                character
            }
        })
        .take(80)
        .collect::<String>();
    let sanitized = sanitized
        .split_at(sanitized.floor_char_boundary(180))
        .0
        .trim_matches([' ', '.']);
    if sanitized.is_empty() {
        fallback.to_string()
    } else {
        sanitized.to_string()
    }
}

fn download_extension(source_format: Option<&str>, transcoded_extension: Option<&str>) -> String {
    let extension = transcoded_extension
        .or(source_format)
        .unwrap_or(AUDIO_EXTENSION)
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .take(12)
        .collect::<String>()
        .to_ascii_lowercase();
    if extension.is_empty() {
        AUDIO_EXTENSION.to_string()
    } else {
        extension
    }
}

fn hash_id(value: &str) -> String {
    hash_id_bytes(value.as_bytes())
}

pub(super) fn hash_id_bytes(value: &[u8]) -> String {
    blake3::hash(value).to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn download_resumes_after_its_private_endpoint_changes() {
        use wiremock::{
            Mock, MockServer, ResponseTemplate,
            matchers::{header, method, path},
        };
        for quality in [
            DownloadQuality::Original,
            DownloadQuality::Transcoded(library::AudioEncoding {
                codec: library::AudioCodec::Opus,
                bitrate_kbps: Some(64),
                channels: library::AudioChannels::Preserve,
            }),
        ] {
            for kind in ["subsonic"] {
                for unchanged in [true, false] {
                    let server = MockServer::start().await;
                    let body = b"complete audio bytes";
                    let offset = 8;
                    let response = if unchanged {
                        ResponseTemplate::new(206)
                            .insert_header("ETag", "\"first\"")
                            .insert_header(
                                "Content-Range",
                                format!("bytes {offset}-{}/{}", body.len() - 1, body.len()),
                            )
                            .set_body_bytes(body[offset..].to_vec())
                    } else {
                        ResponseTemplate::new(200)
                            .insert_header("ETag", "\"changed\"")
                            .set_body_bytes(body.to_vec())
                    };
                    Mock::given(method("GET"))
                        .and(path("/new-private-endpoint"))
                        .and(header("Range", format!("bytes={offset}-").as_str()))
                        .and(header("If-Range", "\"first\""))
                        .respond_with(response)
                        .expect(1)
                        .mount(&server)
                        .await;
                    let source = SourceId::new(format!("{kind}:download"));
                    let media_uri =
                        library::source_entity_uri(&source, "track", "persistent-file-id");
                    let previous = ResolvedStream::with_redacted(
                        "http://127.0.0.1:1/previous-private-endpoint",
                        &media_uri,
                    );
                    let current = ResolvedStream::with_redacted(
                        format!("{}/new-private-endpoint", server.uri()),
                        &media_uri,
                    );
                    let representation = representation_key(
                        Some(&source),
                        &media_uri,
                        quality,
                        previous.redacted_uri(),
                    );
                    assert_eq!(
                        representation,
                        representation_key(
                            Some(&source),
                            &media_uri,
                            quality,
                            current.redacted_uri()
                        )
                    );
                    let stored = format!(
                        "[{},{},{}]",
                        serde_json::to_string(&Some(&source)).unwrap(),
                        format_args!(
                            "{{\"media_uri\":{},\"quality\":{}}}",
                            serde_json::to_string(&media_uri).unwrap(),
                            serde_json::to_string(&quality).unwrap()
                        ),
                        serde_json::to_string(previous.redacted_uri()).unwrap()
                    );
                    assert_eq!(representation, hash_id_bytes(stored.as_bytes()));
                    let directory = tempfile::tempdir().unwrap();
                    let paths = download_paths(directory.path(), Some(&source), &media_uri);
                    std::fs::create_dir_all(&paths.directory).unwrap();
                    std::fs::write(
                        &paths.audio_part,
                        if unchanged {
                            &body[..offset]
                        } else {
                            b"old data"
                        },
                    )
                    .unwrap();
                    std::fs::write(
                        &paths.checkpoint,
                        serde_json::to_vec(&TransferCheckpoint {
                            representation: representation.clone(),
                            validator: "\"first\"".into(),
                            length: body.len() as u64,
                        })
                        .unwrap(),
                    )
                    .unwrap();
                    let (_cancel, mut cancellation) = oneshot::channel();
                    TransferClients::default()
                        .download_cancellable(&current, &representation, &paths, &mut cancellation)
                        .await
                        .unwrap();
                    assert_eq!(std::fs::read(&paths.audio_part).unwrap(), body);
                }
            }
        }
    }
}

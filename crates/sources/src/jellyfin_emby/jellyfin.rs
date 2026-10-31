use super::*;
use serde::Serialize;

impl JellyfinEmbySource {
    pub(crate) fn jellyfin_download(
        &self,
        track: &str,
        quality: crate::DownloadQuality,
        channels: Option<u32>,
    ) -> SourceResult<crate::ResolvedDownload> {
        let original = || {
            stream_descriptor(
                &self.base_url,
                &self.user_id,
                &self.device_id,
                &self.access_token,
                self.trust_invalid_cert,
                track,
                StreamQuality::Original,
            )
        };
        let Some(encoding) = quality.encoding() else {
            return Ok(crate::ResolvedDownload::ready(original()?, None));
        };
        let channels = if encoding.stereo() {
            2
        } else {
            channels.unwrap_or(2)
        };
        let limit = if channels >= 6 { 640 } else { channels * 128 };
        if encoding.codec != library::AudioCodec::Flac
            && encoding.bitrate_kbps.is_some_and(|rate| rate > limit)
        {
            return Ok(crate::ResolvedDownload::encode(original()?, quality));
        }
        let mut url = endpoint(
            &self.base_url,
            &format!("Audio/{}/Universal", raw_item_id(track)),
        )?;
        let container = match encoding.codec {
            library::AudioCodec::Opus | library::AudioCodec::Vorbis => "ogg",
            _ => encoding.codec.name(),
        };
        url.query_pairs_mut()
            .append_pair("UserId", &self.user_id)
            .append_pair("DeviceId", &self.device_id)
            .append_pair("ApiKey", &self.access_token)
            .append_pair("transcodingContainer", container)
            .append_pair("transcodingProtocol", "http")
            .append_pair("audioCodec", encoding.codec.name());
        if encoding.codec != library::AudioCodec::Flac
            && let Some(rate) = encoding.bitrate_kbps
        {
            url.query_pairs_mut()
                .append_pair("audioBitRate", &rate.saturating_mul(1000).to_string());
        }
        if encoding.stereo() {
            url.query_pairs_mut().append_pair("maxAudioChannels", "2");
        }
        let stream = ResolvedStream::new(url.to_string())
            .with_trust_invalid_certificate(self.trust_invalid_cert)
            .with_content_type(Some(encoding.codec.content_type().into()))
            .with_transcoding(true);
        Ok(crate::ResolvedDownload::ready(stream, Some(encoding.codec)))
    }

    pub(crate) async fn jellyfin_create_playlist(
        &self,
        name: &str,
        track_ids: &[String],
        public: Option<bool>,
    ) -> SourceResult<PlaylistId> {
        let url = endpoint(&self.base_url, "Playlists")?;
        let body = CreatePlaylistDto {
            name: name.to_string(),
            ids: raw_track_ids(track_ids),
            user_id: Some(self.user_id.clone()),
            media_type: Some("Audio".to_string()),
            is_public: public.unwrap_or(false),
        };
        let result = self
            .send_json::<PlaylistCreationResult>(self.client.post(url).json(&body))
            .await?;
        Ok(String::from(self.kind.object_id("playlist", &result.id)))
    }

    pub(crate) async fn jellyfin_update_playlist(
        &self,
        playlist_id: &str,
        name: Option<&str>,
        public: Option<bool>,
    ) -> SourceResult<()> {
        let url = endpoint(
            &self.base_url,
            &format!("Playlists/{}", raw_item_id(playlist_id)),
        )?;
        let body = UpdatePlaylistDto {
            name: name.map(str::to_owned),
            is_public: public,
        };
        self.send_unit(self.client.post(url).json(&body)).await
    }

}

pub(super) fn stream_descriptor(
    base_url: &Url,
    user_id: &str,
    device_id: &str,
    access_token: &str,
    trust_invalid_certificate: bool,
    track_object_id: &str,
    quality: StreamQuality,
) -> SourceResult<ResolvedStream> {
    let raw_track_id = raw_item_id(track_object_id);
    let max_bitrate = quality
        .max_bitrate_kbps()
        .map(|kbps| kbps.saturating_mul(1_000).to_string());

    let mut url = endpoint(base_url, &format!("Audio/{raw_track_id}/stream"))?;
    let static_stream = if max_bitrate.is_some() {
        "false"
    } else {
        "true"
    };
    {
        let mut query = url.query_pairs_mut();
        query
            .append_pair("UserId", user_id)
            .append_pair("DeviceId", device_id)
            .append_pair("Static", static_stream)
            .append_pair("ApiKey", access_token);
        if let Some(max_bitrate) = &max_bitrate {
            query
                .append_pair("MaxStreamingBitrate", max_bitrate)
                .append_pair("TranscodingContainer", "mp3")
                .append_pair("AudioCodec", "mp3");
        }
    }
    let mut redacted_url = url.clone();
    {
        let mut redacted_query = redacted_url.query_pairs_mut();
        redacted_query
            .clear()
            .append_pair("UserId", user_id)
            .append_pair("DeviceId", device_id)
            .append_pair("Static", static_stream)
            .append_pair("ApiKey", "<redacted>");
        if let Some(max_bitrate) = &max_bitrate {
            redacted_query
                .append_pair("MaxStreamingBitrate", max_bitrate)
                .append_pair("TranscodingContainer", "mp3")
                .append_pair("AudioCodec", "mp3");
        }
    }
    Ok(
        ResolvedStream::with_redacted(url.to_string(), redacted_url.to_string())
            .with_content_type(max_bitrate.map(|_| "audio/mpeg".to_string()))
            .with_trust_invalid_certificate(trust_invalid_certificate),
    )
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct CreatePlaylistDto {
    pub(super) name: String,
    pub(super) ids: Vec<String>,
    pub(super) user_id: Option<String>,
    pub(super) media_type: Option<String>,
    pub(super) is_public: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct UpdatePlaylistDto {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) is_public: Option<bool>,
}


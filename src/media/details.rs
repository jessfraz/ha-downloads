//! Release observations only. Native paths, credentials and free-form error
//! payloads never become download details or public warning messages.
use crate::media::{
    catalog::queue_acquisition,
    model::*,
    requests::utc_instant,
    service::MediaService,
    upstream::{UpDownloadQuality, UpMovie, UpNamed, UpQueue, UpSeries},
};

impl MediaService {
    pub async fn download_details(&self, id: QueueId) -> Result<DownloadDetails, ApiErrorCode> {
        // Neither manager implements GET queue/{id}. This native endpoint
        // returns the complete queue (including pending releases), unlike a
        // page whose missing ID would not prove that a download disappeared.
        let records = self
            .manager(id.manager())
            .get::<Vec<UpQueue>>(
                "queue/details",
                &[
                    ("includeMovie", "true".into()),
                    ("includeSeries", "true".into()),
                    ("includeEpisode", "true".into()),
                ],
            )
            .await?;
        if records.len() > 1000 {
            return Err(ApiErrorCode::InvalidUpstreamResponse);
        }
        let mut found = records.iter().filter(|record| record.id == id.native_id());
        let record = found.next().ok_or(ApiErrorCode::NotFound)?;
        if found.next().is_some() {
            return Err(ApiErrorCode::InvalidUpstreamResponse);
        }
        let media = match id.manager() {
            ManagerId::Radarr => record
                .movie
                .as_ref()
                .and_then(|movie| TmdbId::try_from(movie.tmdb_id).ok())
                .map(|tmdb_id| MediaIdentity::Movie { tmdb_id }),
            ManagerId::Sonarr => record
                .series
                .as_ref()
                .and_then(|series| TvdbId::try_from(series.tvdb_id).ok())
                .map(|tvdb_id| MediaIdentity::Series { tvdb_id }),
        }
        .ok_or(ApiErrorCode::InvalidUpstreamResponse)?;
        let quality = record.quality.as_ref().and_then(quality);
        let import_status = match record.tracked_download_state.as_str() {
            "downloading" => DownloadImportStatus::NotStarted,
            "importPending" => DownloadImportStatus::Pending,
            "importing" => DownloadImportStatus::Importing,
            "imported" => DownloadImportStatus::Imported,
            "importBlocked" => DownloadImportStatus::Failed,
            _ => DownloadImportStatus::Unknown,
        };
        Ok(DownloadDetails {
            id,
            manager: id.manager(),
            media,
            title: label(&record.title).unwrap_or_else(|| "Download".into()),
            episode: record.episode.as_ref().map(|episode| EpisodeNumber {
                season: episode.season_number,
                number: episode.episode_number,
            }),
            acquisition: queue_acquisition(record),
            status: match record.status.as_str() {
                "queued" => DownloadStatus::Queued,
                "paused" => DownloadStatus::Paused,
                "downloading" => DownloadStatus::Downloading,
                "completed" => DownloadStatus::Completed,
                "failed" => DownloadStatus::Failed,
                _ => DownloadStatus::Unknown,
            },
            quality,
            progress: progress(record),
            remaining_seconds: record.timeleft.as_deref().and_then(remaining_seconds),
            download_client: record.download_client.as_deref().and_then(label),
            indexer: record.indexer.as_deref().and_then(label),
            protocol: record.protocol.as_deref().map(|protocol| match protocol {
                "usenet" => DownloadProtocol::Usenet,
                "torrent" => DownloadProtocol::Torrent,
                _ => DownloadProtocol::Unknown,
            }),
            import_status,
            warnings: warnings(record, import_status),
            languages: names(&record.languages),
            custom_formats: names(&record.custom_formats),
            custom_format_score: record.custom_format_score,
            added_at: timestamp(record.added.as_deref()),
            estimated_completion_at: timestamp(record.estimated_completion_time.as_deref()),
            observed_at_unix_ms: crate::monitor::unix_ms(),
        })
    }
}
pub(crate) fn quality(model: &UpDownloadQuality) -> Option<DownloadQuality> {
    Some(DownloadQuality {
        name: label(&model.quality.name)?,
        resolution: model
            .quality
            .resolution
            .filter(|value| *value > 0 && *value <= 4320),
        source: model.quality.source.as_deref().and_then(label),
        modifier: model.quality.modifier.as_deref().and_then(label),
        revision: model.revision.as_ref().map(|revision| DownloadRevision {
            version: revision.version,
            real: revision.real,
            is_repack: revision.is_repack,
        }),
    })
}
pub(crate) fn movie_metadata(movie: &UpMovie) -> Option<MovieMetadata> {
    let native = &movie.metadata;
    let metadata = MovieMetadata {
        runtime_minutes: native.runtime.filter(|value| *value > 0),
        genres: native
            .genres
            .iter()
            .take(64)
            .filter_map(|value| label(value))
            .collect(),
        studio: native.studio.as_deref().and_then(label),
        certification: native.certification.as_deref().and_then(label),
        file: native
            .movie_file
            .as_ref()
            .filter(|_| movie.has_file || movie.movie_file_id > 0)
            .map(|file| MediaFileDetails {
                quality: file.quality.as_ref().and_then(quality),
                size_bytes: file.size.filter(|value| *value > 0),
                video_codec: file
                    .media_info
                    .as_ref()
                    .and_then(|info| info.video_codec.as_deref())
                    .and_then(label),
                audio_codec: file
                    .media_info
                    .as_ref()
                    .and_then(|info| info.audio_codec.as_deref())
                    .and_then(label),
                audio_channels: file
                    .media_info
                    .as_ref()
                    .and_then(|info| info.audio_channels)
                    .filter(|value| value.is_finite() && *value > 0.0 && *value <= 64.0)
                    .map(|value| value.to_string()),
            }),
    };
    (metadata.runtime_minutes.is_some()
        || !metadata.genres.is_empty()
        || metadata.studio.is_some()
        || metadata.certification.is_some()
        || metadata.file.is_some())
    .then_some(metadata)
}
pub(crate) fn series_metadata(series: &UpSeries) -> Option<SeriesMetadata> {
    let native = &series.metadata;
    let metadata = SeriesMetadata {
        runtime_minutes: native.runtime.filter(|value| *value > 0),
        genres: native
            .genres
            .iter()
            .take(64)
            .filter_map(|value| label(value))
            .collect(),
        network: native.network.as_deref().and_then(label),
        certification: native.certification.as_deref().and_then(label),
        next_airing: timestamp(native.next_airing.as_deref()),
    };
    (metadata.runtime_minutes.is_some()
        || !metadata.genres.is_empty()
        || metadata.network.is_some()
        || metadata.certification.is_some()
        || metadata.next_airing.is_some())
    .then_some(metadata)
}
pub(crate) fn progress(record: &UpQueue) -> Option<DownloadProgress> {
    let (total, remaining) = record.size.zip(record.sizeleft)?;
    if !total.is_finite()
        || !remaining.is_finite()
        || total < 1.0
        || remaining < 0.0
        || remaining > total
        || total >= u64::MAX as f64
    {
        return None;
    }
    let total_bytes = total as u64;
    let remaining_bytes = remaining as u64;
    Some(DownloadProgress {
        completed_bytes: total_bytes - remaining_bytes,
        total_bytes,
        remaining_bytes,
    })
}
fn remaining_seconds(value: &str) -> Option<u64> {
    let mut parts = value.split(':');
    let hours = parts.next()?;
    let minutes = parts.next()?.parse::<u64>().ok()?;
    let seconds = parts.next()?;
    if parts.next().is_some() || minutes >= 60 {
        return None;
    }
    let (days, hours) = if let Some((days, hours)) = hours.split_once('.') {
        (days.parse::<u64>().ok()?, hours.parse::<u64>().ok()?)
    } else {
        (0, hours.parse::<u64>().ok()?)
    };
    let seconds = if let Some((seconds, fraction)) = seconds.split_once('.') {
        if fraction.is_empty()
            || fraction.len() > 7
            || !fraction.bytes().all(|byte| byte.is_ascii_digit())
        {
            return None;
        }
        seconds
    } else {
        seconds
    };
    let seconds = seconds.parse::<u64>().ok()?;
    if hours >= 24 || seconds >= 60 {
        return None;
    }
    days.checked_mul(86_400)?
        .checked_add(hours * 3600 + minutes * 60 + seconds)
}
fn timestamp(value: Option<&str>) -> Option<String> {
    value
        .filter(|value| utc_instant(value).is_some())
        .map(str::to_owned)
}
pub(crate) fn label(value: &str) -> Option<String> {
    let value = value.trim();
    let lower = value.to_ascii_lowercase();
    if value.is_empty()
        || value.chars().count() > 250
        || value.chars().any(char::is_control)
        || value.contains("://")
        || value.contains('\\')
        || value.split_whitespace().any(|part| part.starts_with('/'))
        || ["api_key", "apikey", "password", "bearer ", "token="]
            .iter()
            .any(|part| lower.contains(part))
    {
        return None;
    }
    Some(value.to_owned())
}
fn names(values: &[UpNamed]) -> Vec<String> {
    let mut names = vec![];
    for value in values
        .iter()
        .take(64)
        .filter_map(|value| label(&value.name))
    {
        if !names.contains(&value) {
            names.push(value);
        }
    }
    names
}
fn warnings(record: &UpQueue, import: DownloadImportStatus) -> Vec<DownloadWarning> {
    let mut warnings = vec![];
    let warning = match import {
        _ if record.tracked_download_state == "importBlocked" => Some((
            DownloadWarningCode::ImportBlocked,
            if record.tracked_download_status == "error" {
                IssueSeverity::Error
            } else {
                IssueSeverity::Warning
            },
            "The download needs attention before it can be imported.",
        )),
        _ if matches!(
            record.tracked_download_state.as_str(),
            "failedPending" | "failed"
        ) =>
        {
            Some((
                DownloadWarningCode::DownloadFailed,
                IssueSeverity::Error,
                "The manager reported a download failure.",
            ))
        }
        DownloadImportStatus::Pending
            if record.tracked_download_status == "warning"
                || record.tracked_download_status == "error" =>
        {
            Some((
                DownloadWarningCode::ImportBlocked,
                IssueSeverity::Warning,
                "The download needs attention before it can be imported.",
            ))
        }
        _ if record.status == "failed" || record.tracked_download_status == "error" => Some((
            DownloadWarningCode::DownloadFailed,
            IssueSeverity::Error,
            "The manager reported a download failure.",
        )),
        _ if record
            .error_message
            .as_ref()
            .is_some_and(|message| !message.is_empty()) =>
        {
            Some((
                DownloadWarningCode::ProviderError,
                IssueSeverity::Error,
                "The download client reported an error. Open its native app for details.",
            ))
        }
        _ if record.tracked_download_status == "warning" || !record.status_messages.is_empty() => {
            Some((
                DownloadWarningCode::ProviderWarning,
                IssueSeverity::Warning,
                "The manager reported a warning. Open its native app for details.",
            ))
        }
        _ => None,
    };
    if let Some((code, severity, message)) = warning {
        warnings.push(DownloadWarning {
            code,
            severity,
            message: message.into(),
        });
    }
    warnings
}

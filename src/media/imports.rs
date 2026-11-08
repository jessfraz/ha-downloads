//! Successful native imports, including work initiated outside Downloads.
//! Native history is the completion authority; transfer and request receipts
//! cannot create an import event. The recent window is bounded and its coverage
//! explicitly reports any older history outside that window.
use crate::media::{
    catalog::coverage,
    details::{label, quality},
    model::*,
    plex::{Plex, ProofInput},
    requests::utc_instant,
    service::{MediaService, page},
    upstream::{UpDownloadQuality, UpMovie, UpSeries},
};
use serde::{Deserialize, Serialize};
use std::time::Duration;

const WINDOW: usize = 1000;
const SCOPE: &str = "imports-v1";
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NativeImport {
    pub id: u32,
    pub event_type: String,
    pub date: String,
    #[serde(default)]
    pub quality: Option<UpDownloadQuality>,
    #[serde(default)]
    pub movie: Option<UpMovie>,
    #[serde(default)]
    pub series: Option<UpSeries>,
    #[serde(default)]
    pub episode: Option<NativeImportEpisode>,
    #[serde(default)]
    pub data: NativeImportData,
}
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct NativeImportData {
    pub imported_path: Option<String>,
    pub size: Option<String>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NativeImportEpisode {
    pub season_number: u16,
    pub episode_number: u16,
    #[serde(default)]
    pub tvdb_id: Option<u32>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NativeHistoryPage {
    records: Vec<NativeImport>,
    total_records: u32,
}
struct ImportObservation {
    item: ImportedItem,
    proof: ProofInput,
}
impl MediaService {
    async fn native_imports(&self, manager: ManagerId) -> (Vec<NativeImport>, SourceCoverage) {
        let component = match manager {
            ManagerId::Radarr => Component::Radarr,
            ManagerId::Sonarr => Component::Sonarr,
        };
        let response = self
            .manager(manager)
            .get::<NativeHistoryPage>(
                "history",
                &[
                    ("page", "1".into()),
                    ("pageSize", WINDOW.to_string()),
                    ("sortKey", "date".into()),
                    ("sortDirection", "descending".into()),
                    ("eventType", "3".into()),
                    ("includeMovie", "true".into()),
                    ("includeSeries", "true".into()),
                    ("includeEpisode", "true".into()),
                ],
            )
            .await;
        if let Ok(response) = response
            && response.records.len() <= WINDOW
            && response.total_records as usize >= response.records.len()
        {
            let at = crate::monitor::unix_ms();
            let partial = response.total_records as usize > response.records.len();
            self.cache
                .write()
                .await
                .imports
                .insert(manager, (response.records.clone(), at));
            let mut source = coverage(component, true, at);
            if partial {
                source.state = CoverageState::Partial;
            }
            return (response.records, source);
        }
        let cached = self.cache.read().await.imports.get(&manager).cloned();
        match cached {
            Some((records, at)) => (records, coverage(component, false, at)),
            None => (vec![], coverage(component, false, 0)),
        }
    }
    pub async fn imports(
        &self,
        limit: Option<u32>,
        token: Option<&str>,
        since: Option<&str>,
    ) -> Result<Page<ImportedItem>, ApiErrorCode> {
        let since = since
            .map(|value| utc_instant(value).ok_or(ApiErrorCode::InvalidArguments))
            .transpose()?;
        let scope = match since {
            Some((year, month, day, hour, minute, second, nanos)) => format!(
                "{SCOPE}:{year:04}{month:02}{day:02}{hour:02}{minute:02}{second:02}{nanos:09}"
            ),
            None => SCOPE.to_owned(),
        };
        // Validate scoped cursors before any provider read.
        page::<ImportObservation>(
            vec![],
            limit,
            token,
            &scope,
            |row| sort_key(&row.item),
            vec![],
        )?;
        let (movies, series) = tokio::join!(
            self.native_imports(ManagerId::Radarr),
            self.native_imports(ManagerId::Sonarr)
        );
        let mut observations = vec![];
        let mut sources = vec![];
        for (manager, (records, mut source)) in
            [(ManagerId::Radarr, movies), (ManagerId::Sonarr, series)]
        {
            let mut seen = std::collections::BTreeSet::new();
            for record in records {
                if since
                    .zip(utc_instant(&record.date))
                    .is_some_and(|(since, imported)| imported < since)
                {
                    continue;
                }
                if !seen.insert(record.id) {
                    if source.state == CoverageState::Available {
                        source.state = CoverageState::Partial;
                    }
                    continue;
                }
                match observation(
                    manager,
                    record,
                    source.observed_at_unix_ms,
                    !self.config.plex.url.is_empty(),
                ) {
                    Some(observation) => observations.push(observation),
                    None if source.state == CoverageState::Available => {
                        source.state = CoverageState::Partial
                    }
                    None => {}
                }
            }
            sources.push(source);
        }
        let selected = page(
            observations,
            limit,
            token,
            &scope,
            |row| sort_key(&row.item),
            sources,
        )?;
        let mut rows = selected.items;
        if !rows.is_empty() && !self.config.plex.url.is_empty() {
            match Plex::new(&self.config.plex, &self.egress) {
                Ok(plex) => {
                    let check = async {
                        match plex.available().await {
                            Ok(()) => {
                                for row in &mut rows {
                                    (row.item.plex, row.item.subtitles) =
                                        plex.verify(&row.proof).await;
                                }
                            }
                            Err(reason) => {
                                for row in &mut rows {
                                    row.item.plex = PlexImportStatus::Unavailable { reason };
                                }
                            }
                        }
                    };
                    // A slow optional library must not stall native completion
                    // history. Unfinished observations retain explicit timeout.
                    let _ = tokio::time::timeout(Duration::from_secs(8), check).await;
                }
                Err(reason) => {
                    for row in &mut rows {
                        row.item.plex = PlexImportStatus::Unavailable { reason };
                    }
                }
            }
        }
        Ok(Page {
            items: rows.into_iter().map(|row| row.item).collect(),
            next_page: selected.next_page,
            coverage: selected.coverage,
        })
    }
}
fn observation(
    manager: ManagerId,
    record: NativeImport,
    at: u64,
    plex_configured: bool,
) -> Option<ImportObservation> {
    if record.event_type != "downloadFolderImported" || utc_instant(&record.date).is_none() {
        return None;
    }
    let id = ImportId::new(manager, record.id).ok()?;
    let (media, title, guid, plex_type) = match manager {
        ManagerId::Radarr => {
            let movie = record.movie.as_ref()?;
            let tmdb_id = TmdbId::try_from(movie.tmdb_id).ok()?;
            (
                MediaIdentity::Movie { tmdb_id },
                label(&movie.title)?,
                Some(format!("tmdb://{tmdb_id}")),
                1,
            )
        }
        ManagerId::Sonarr => {
            let series = record.series.as_ref()?;
            let tvdb_id = TvdbId::try_from(series.tvdb_id).ok()?;
            let episode_guid = record
                .episode
                .as_ref()
                .and_then(|episode| episode.tvdb_id)
                .and_then(|id| TvdbId::try_from(id).ok())
                .map(|id| format!("tvdb://{id}"));
            (
                MediaIdentity::Series { tvdb_id },
                label(&series.title)?,
                episode_guid,
                4,
            )
        }
    };
    let episode = record.episode.as_ref().map(|episode| EpisodeNumber {
        season: episode.season_number,
        number: episode.episode_number,
    });
    let proof = ProofInput {
        guid,
        plex_type,
        episode: episode.clone(),
        imported_at: record.date.clone(),
        path: record.data.imported_path,
        size: record
            .data
            .size
            .as_deref()
            .and_then(|size| size.parse::<u64>().ok())
            .filter(|size| *size > 0),
    };
    Some(ImportObservation {
        item: ImportedItem {
            id,
            manager,
            media,
            title,
            episode,
            imported_at: record.date,
            quality: record.quality.as_ref().and_then(quality),
            plex: if plex_configured {
                PlexImportStatus::Unavailable {
                    reason: UnavailableReason::Timeout,
                }
            } else {
                PlexImportStatus::NotConfigured
            },
            subtitles: SubtitleImportStatus::Unknown {
                reason: SubtitleUnknownReason::PlexUnverified,
            },
            observed_at_unix_ms: at,
        },
        proof,
    })
}
fn sort_key(item: &ImportedItem) -> String {
    match utc_instant(&item.imported_at) {
        Some((year, month, day, hour, minute, second, nanos)) => format!(
            "{:04}{:02}{:02}{:02}{:02}{:02}{:09}:{}:{:010}",
            9999 - year,
            12 - month,
            31 - day,
            23 - hour,
            59 - minute,
            59 - second,
            999_999_999 - nanos,
            item.manager,
            i32::MAX as u32 - item.id.native_id(),
        ),
        None => item.id.to_string(),
    }
}

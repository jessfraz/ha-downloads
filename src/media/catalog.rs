use crate::media::{
    model::*,
    service::{MediaService, page, safe_text},
    upstream::*,
};

impl MediaService {
    pub(crate) async fn movie_records(&self) -> (Vec<UpMovie>, SourceCoverage) {
        match self.radarr.get::<Vec<UpMovie>>("movie", &[]).await {
            Ok(items) if items.len() <= 10000 => {
                let now = crate::monitor::unix_ms();
                let mut cache = self.cache.write().await;
                cache.movies = items.clone();
                cache.movie_at = now;
                (items, coverage(Component::Radarr, true, now))
            }
            _ => {
                let cache = self.cache.read().await;
                (
                    cache.movies.clone(),
                    coverage(Component::Radarr, false, cache.movie_at),
                )
            }
        }
    }
    pub(crate) async fn series_records(&self) -> (Vec<UpSeries>, SourceCoverage) {
        match self.sonarr.get::<Vec<UpSeries>>("series", &[]).await {
            Ok(items) if items.len() <= 10000 => {
                let now = crate::monitor::unix_ms();
                let mut cache = self.cache.write().await;
                cache.series = items.clone();
                cache.series_at = now;
                (items, coverage(Component::Sonarr, true, now))
            }
            _ => {
                let cache = self.cache.read().await;
                (
                    cache.series.clone(),
                    coverage(Component::Sonarr, false, cache.series_at),
                )
            }
        }
    }
    pub async fn movies(
        &self,
        limit: Option<u32>,
        token: Option<&str>,
    ) -> Result<Page<MediaItem>, ApiErrorCode> {
        let (movies, coverage) = self.movie_records().await;
        let profiles = self.profiles(Some(MediaKind::Movie)).await.items;
        let items = movies
            .into_iter()
            .map(|movie| movie_item(movie, &profiles))
            .collect::<Result<Vec<_>, _>>()?;
        page(items, limit, token, "movies", media_key, vec![coverage])
    }
    pub async fn series_list(
        &self,
        limit: Option<u32>,
        token: Option<&str>,
    ) -> Result<Page<MediaItem>, ApiErrorCode> {
        let (series, coverage) = self.series_records().await;
        let profiles = self.profiles(Some(MediaKind::Series)).await.items;
        let items = series
            .into_iter()
            .map(|series| series_item(series, &profiles))
            .collect::<Result<Vec<_>, _>>()?;
        page(items, limit, token, "series", media_key, vec![coverage])
    }
    pub(crate) async fn find_movie(&self, id: TmdbId) -> Result<UpMovie, ApiErrorCode> {
        let existing = self
            .radarr
            .get::<Vec<UpMovie>>("movie", &[("tmdbId", id.to_string())])
            .await?;
        if let Some(movie) = existing.into_iter().find(|m| m.tmdb_id == id.get()) {
            return Ok(movie);
        }
        self.radarr
            .get::<Vec<UpMovie>>("movie/lookup", &[("term", format!("tmdb:{id}"))])
            .await?
            .into_iter()
            .find(|m| m.tmdb_id == id.get())
            .ok_or(ApiErrorCode::NotFound)
    }
    pub(crate) async fn find_series(&self, id: TvdbId) -> Result<UpSeries, ApiErrorCode> {
        let existing = self
            .sonarr
            .get::<Vec<UpSeries>>("series", &[("tvdbId", id.to_string())])
            .await?;
        if let Some(series) = existing.into_iter().find(|s| s.tvdb_id == id.get()) {
            return Ok(series);
        }
        self.sonarr
            .get::<Vec<UpSeries>>("series/lookup", &[("term", format!("tvdb:{id}"))])
            .await?
            .into_iter()
            .find(|s| s.tvdb_id == id.get())
            .ok_or(ApiErrorCode::NotFound)
    }
    pub async fn movie(&self, id: TmdbId) -> Result<MediaItem, ApiErrorCode> {
        let movie = self.find_movie(id).await?;
        movie_item(movie, &self.profiles(Some(MediaKind::Movie)).await.items)
    }
    pub async fn series(&self, id: TvdbId) -> Result<MediaItem, ApiErrorCode> {
        let series = self.find_series(id).await?;
        let native_id = series.id;
        let mut item = series_item(series, &self.profiles(Some(MediaKind::Series)).await.items)?;
        if native_id != 0 {
            let episodes = self
                .sonarr
                .get::<Vec<UpEpisode>>("episode", &[("seriesId", native_id.to_string())])
                .await?;
            let (queue, queue_coverage) = self.queue_records(ManagerId::Sonarr).await;
            if let MediaItem::Series {
                seasons,
                acquisition,
                ..
            } = &mut item
            {
                for season in seasons.iter_mut() {
                    let selected = episodes
                        .iter()
                        .filter(|e| e.season_number == season.number)
                        .collect::<Vec<_>>();
                    let imported = selected.iter().filter(|e| e.has_file).count() as u32;
                    let active = selected
                        .iter()
                        .filter(|e| queue.iter().any(|q| q.episode_id == Some(e.id)))
                        .count() as u32;
                    season.episode_count = Some(selected.len() as u32);
                    season.imported_count = Some(imported);
                    season.active_count =
                        (queue_coverage.state == CoverageState::Available).then_some(active);
                    season.state = if imported == selected.len() as u32 && imported > 0 {
                        SeasonState::Imported
                    } else if queue_coverage.state == CoverageState::Available {
                        season_state(selected.len() as u32, imported, active)
                    } else {
                        SeasonState::Unknown
                    };
                }
                *acquisition = if !seasons.is_empty()
                    && seasons.iter().all(|s| s.state == SeasonState::Imported)
                {
                    Acquisition::Imported
                } else if queue_coverage.state == CoverageState::Unavailable {
                    Acquisition::Unavailable {
                        component: Component::Sonarr,
                    }
                } else if let Some(current) = queue.iter().find(|q| q.series_id == Some(native_id))
                {
                    queue_acquisition(current)
                } else if queue_coverage.state != CoverageState::Available {
                    Acquisition::Unknown
                } else {
                    Acquisition::Waiting
                };
            }
        }
        Ok(item)
    }
    pub async fn episodes(
        &self,
        id: TvdbId,
        season: Option<u16>,
        limit: Option<u32>,
        token: Option<&str>,
    ) -> Result<Page<Episode>, ApiErrorCode> {
        let series = self.find_series(id).await?;
        if series.id == 0 {
            return Ok(Page {
                items: vec![],
                next_page: None,
                coverage: vec![SourceCoverage {
                    source: Component::Sonarr,
                    state: CoverageState::Unsupported,
                    observed_at_unix_ms: crate::monitor::unix_ms(),
                }],
            });
        }
        let episodes = self
            .sonarr
            .get::<Vec<UpEpisode>>("episode", &[("seriesId", series.id.to_string())])
            .await?;
        let (queue, queue_coverage) = self.queue_records(ManagerId::Sonarr).await;
        let items = episodes
            .into_iter()
            .filter(|e| season.is_none_or(|s| s == e.season_number))
            .map(|episode| {
                let acquisition = if episode.has_file {
                    Acquisition::Imported
                } else if queue_coverage.state == CoverageState::Unavailable {
                    Acquisition::Unavailable {
                        component: Component::Sonarr,
                    }
                } else if let Some(q) = queue.iter().find(|q| q.episode_id == Some(episode.id)) {
                    queue_acquisition(q)
                } else if queue_coverage.state != CoverageState::Available {
                    Acquisition::Unknown
                } else {
                    Acquisition::Waiting
                };
                Ok(Episode {
                    id: EpisodeId::try_from(episode.id)
                        .map_err(|_| ApiErrorCode::InvalidUpstreamResponse)?,
                    season: episode.season_number,
                    number: episode.episode_number,
                    title: safe_text(&episode.title, 200),
                    air_date: episode.air_date,
                    monitored: episode.monitored,
                    acquisition,
                })
            })
            .collect::<Result<Vec<_>, ApiErrorCode>>()?;
        page(
            items,
            limit,
            token,
            &format!("episodes:{id}:{season:?}"),
            |e| format!("{:05}:{:05}:{:010}", e.season, e.number, e.id.get()),
            vec![SourceCoverage {
                source: Component::Sonarr,
                state: if queue_coverage.state == CoverageState::Available {
                    CoverageState::Available
                } else {
                    CoverageState::Partial
                },
                observed_at_unix_ms: if queue_coverage.state == CoverageState::Available {
                    crate::monitor::unix_ms()
                } else {
                    queue_coverage.observed_at_unix_ms
                },
            }],
        )
    }
    pub async fn search(
        &self,
        query: SearchQuery,
        kind: SearchKind,
        limit: Option<u32>,
        token: Option<&str>,
    ) -> Result<Page<MediaItem>, ApiErrorCode> {
        let profiles = self.profiles(None).await.items;
        let movie = async {
            if kind == SearchKind::Series {
                return Ok(vec![]);
            }
            self.radarr
                .get::<Vec<UpMovie>>("movie/lookup", &[("term", query.as_str().to_owned())])
                .await
        };
        let series = async {
            if kind == SearchKind::Movie {
                return Ok(vec![]);
            }
            self.sonarr
                .get::<Vec<UpSeries>>("series/lookup", &[("term", query.as_str().to_owned())])
                .await
        };
        let (movies, series) = tokio::join!(movie, series);
        let now = crate::monitor::unix_ms();
        let mut items = Vec::new();
        let mut sources = Vec::new();
        for (component, result) in [
            (
                Component::Radarr,
                movies.map(|v| {
                    v.into_iter()
                        .map(|m| movie_item(m, &profiles))
                        .collect::<Result<Vec<_>, _>>()
                }),
            ),
            (
                Component::Sonarr,
                series.map(|v| {
                    v.into_iter()
                        .map(|s| series_item(s, &profiles))
                        .collect::<Result<Vec<_>, _>>()
                }),
            ),
        ] {
            if (component == Component::Radarr && kind == SearchKind::Series)
                || (component == Component::Sonarr && kind == SearchKind::Movie)
            {
                continue;
            }
            let cache_key = format!("{component}:{}", query.as_str());
            match result.and_then(|v| v) {
                Ok(found) => {
                    items.extend(found.clone());
                    let mut cache = self.cache.write().await;
                    if cache.searches.len() >= 32 {
                        cache.searches.clear()
                    }
                    cache.searches.insert(cache_key, (found, now));
                    sources.push(coverage(component, true, now));
                }
                Err(_) => {
                    let cache = self.cache.read().await;
                    let at = if let Some((found, at)) = cache.searches.get(&cache_key) {
                        items.extend(found.clone());
                        *at
                    } else {
                        0
                    };
                    sources.push(coverage(component, false, at));
                }
            }
        }
        page(
            items,
            limit,
            token,
            &format!("search:{kind}:{}", query.as_str()),
            media_key,
            sources,
        )
    }
    pub(crate) async fn queue_records(&self, manager: ManagerId) -> (Vec<UpQueue>, SourceCoverage) {
        let component = match manager {
            ManagerId::Radarr => Component::Radarr,
            ManagerId::Sonarr => Component::Sonarr,
        };
        let result = self
            .manager(manager)
            .get::<UpQueuePage>(
                "queue",
                &[
                    ("pageSize", "1000".into()),
                    ("includeMovie", "true".into()),
                    ("includeSeries", "true".into()),
                    ("includeEpisode", "true".into()),
                ],
            )
            .await;
        match result {
            Ok(page) if page.records.len() <= 1000 => {
                let now = crate::monitor::unix_ms();
                let truncated = page.total_records as usize > page.records.len();
                let mut cache = self.cache.write().await;
                match manager {
                    ManagerId::Radarr => {
                        cache.movie_queue = page.records.clone();
                        cache.movie_queue_at = now
                    }
                    ManagerId::Sonarr => {
                        cache.series_queue = page.records.clone();
                        cache.series_queue_at = now
                    }
                }
                let mut cov = coverage(component, true, now);
                if truncated {
                    cov.state = CoverageState::Partial
                }
                (page.records, cov)
            }
            _ => {
                let cache = self.cache.read().await;
                match manager {
                    ManagerId::Radarr => (
                        cache.movie_queue.clone(),
                        coverage(component, false, cache.movie_queue_at),
                    ),
                    ManagerId::Sonarr => (
                        cache.series_queue.clone(),
                        coverage(component, false, cache.series_queue_at),
                    ),
                }
            }
        }
    }
    pub async fn queue(
        &self,
        limit: Option<u32>,
        token: Option<&str>,
    ) -> Result<Page<QueueItem>, ApiErrorCode> {
        let (movies, series) = tokio::join!(
            self.queue_records(ManagerId::Radarr),
            self.queue_records(ManagerId::Sonarr)
        );
        let mut items = vec![];
        for (manager, (records, source)) in [
            (ManagerId::Radarr, movies.clone()),
            (ManagerId::Sonarr, series.clone()),
        ] {
            for record in records {
                let media = match manager {
                    ManagerId::Radarr => record
                        .movie
                        .as_ref()
                        .and_then(|m| TmdbId::try_from(m.tmdb_id).ok())
                        .map(|tmdb_id| MediaIdentity::Movie { tmdb_id }),
                    ManagerId::Sonarr => record
                        .series
                        .as_ref()
                        .and_then(|s| TvdbId::try_from(s.tvdb_id).ok())
                        .map(|tvdb_id| MediaIdentity::Series { tvdb_id }),
                };
                let Some(media) = media else {
                    if source.state == CoverageState::Available {
                        return Err(ApiErrorCode::InvalidUpstreamResponse);
                    }
                    continue;
                };
                items.push(QueueItem {
                    id: format!("{manager}:{}", record.id),
                    media,
                    title: safe_text(&record.title, 250),
                    episode: record.episode.as_ref().map(|e| EpisodeNumber {
                        season: e.season_number,
                        number: e.episode_number,
                    }),
                    acquisition: queue_acquisition(&record),
                });
            }
        }
        page(
            items,
            limit,
            token,
            "queue",
            |q| q.id.clone(),
            vec![movies.1, series.1],
        )
    }
    pub async fn artwork(
        &self,
        kind: MediaKind,
        id: u32,
        artwork: ArtworkKind,
    ) -> Result<(String, Vec<u8>), ApiErrorCode> {
        let (manager, images) = match kind {
            MediaKind::Movie => (
                ManagerId::Radarr,
                self.find_movie(TmdbId::try_from(id).map_err(|_| ApiErrorCode::InvalidArguments)?)
                    .await?
                    .images,
            ),
            MediaKind::Series => (
                ManagerId::Sonarr,
                self.find_series(TvdbId::try_from(id).map_err(|_| ApiErrorCode::InvalidArguments)?)
                    .await?
                    .images,
            ),
        };
        let expected = match artwork {
            ArtworkKind::Poster => "poster",
            ArtworkKind::Backdrop => "fanart",
        };
        let image = images
            .iter()
            .find(|i| i.cover_type.eq_ignore_ascii_case(expected))
            .ok_or(ApiErrorCode::NotFound)?;
        // Remote artwork is allowed only while the existing runtime has fresh protection proof.
        let status = self.status.borrow().clone();
        let fresh = status.diagnostics.checked_at_unix_ms.is_some_and(|at| {
            crate::monitor::unix_ms()
                .checked_sub(at)
                .is_some_and(|age| age <= 30000)
        });
        let allow_remote = fresh
            && status.vpn.state == crate::status::VpnState::Connected
            && status.diagnostics.checks.vpn_health.passed()
            && status.diagnostics.checks.vpn_service.passed()
            && status.protection.kill_switch == crate::status::Verification::Verified
            && status.protection.dns == crate::status::Verification::Verified;
        self.manager(manager).artwork(image, allow_remote).await
    }
}
pub(crate) fn coverage(source: Component, available: bool, at: u64) -> SourceCoverage {
    SourceCoverage {
        source,
        state: if available {
            CoverageState::Available
        } else {
            CoverageState::Unavailable
        },
        observed_at_unix_ms: at,
    }
}
pub(crate) fn media_key(item: &MediaItem) -> String {
    match item {
        MediaItem::Movie { tmdb_id, .. } => format!("movie:{:010}", tmdb_id.get()),
        MediaItem::Series { tvdb_id, .. } => format!("series:{:010}", tvdb_id.get()),
    }
}
fn quality_name(id: u32, profiles: &[DownloadProfile]) -> String {
    profiles
        .iter()
        .find_map(|p| match p {
            DownloadProfile::Movie { name, settings, .. }
                if settings.quality_profile_id.get() == id =>
            {
                Some(name.as_str().to_owned())
            }
            DownloadProfile::Series { name, settings, .. }
                if settings.quality_profile_id.get() == id =>
            {
                Some(name.as_str().to_owned())
            }
            _ => None,
        })
        .unwrap_or_else(|| format!("Quality profile {id}"))
}
fn movie_item(movie: UpMovie, profiles: &[DownloadProfile]) -> Result<MediaItem, ApiErrorCode> {
    let id = TmdbId::try_from(movie.tmdb_id).map_err(|_| ApiErrorCode::InvalidUpstreamResponse)?;
    let library = if movie.id == 0 {
        LibraryState::NotAdded
    } else {
        LibraryState::Managed {
            upstream_id: movie.id,
            quality_profile_id: QualityProfileId::try_from(movie.quality_profile_id)
                .map_err(|_| ApiErrorCode::InvalidUpstreamResponse)?,
            quality_profile_name: quality_name(movie.quality_profile_id, profiles),
        }
    };
    let default_profile = profiles.iter().find_map(|p| match p {
        DownloadProfile::Movie {
            id,
            revision,
            is_default: true,
            ..
        } => Some(MovieProfileRef {
            id: id.clone(),
            revision: *revision,
        }),
        _ => None,
    });
    Ok(MediaItem::Movie {
        tmdb_id: id,
        title: safe_text(&movie.title, 200),
        year: (movie.year > 0).then_some(movie.year),
        overview: safe_text(&movie.overview, 2000),
        poster: (!movie.images.is_empty()).then(|| format!("api/v1/artwork/movie/{id}/poster")),
        default_profile,
        library,
        acquisition: if movie.has_file || movie.movie_file_id > 0 {
            Acquisition::Imported
        } else if movie.id > 0 {
            Acquisition::Waiting
        } else {
            Acquisition::NotRequested
        },
    })
}
fn series_item(series: UpSeries, profiles: &[DownloadProfile]) -> Result<MediaItem, ApiErrorCode> {
    let id = TvdbId::try_from(series.tvdb_id).map_err(|_| ApiErrorCode::InvalidUpstreamResponse)?;
    let library = if series.id == 0 {
        LibraryState::NotAdded
    } else {
        LibraryState::Managed {
            upstream_id: series.id,
            quality_profile_id: QualityProfileId::try_from(series.quality_profile_id)
                .map_err(|_| ApiErrorCode::InvalidUpstreamResponse)?,
            quality_profile_name: quality_name(series.quality_profile_id, profiles),
        }
    };
    let default_profile = profiles.iter().find_map(|p| match p {
        DownloadProfile::Series {
            id,
            revision,
            is_default: true,
            ..
        } => Some(SeriesProfileRef {
            id: id.clone(),
            revision: *revision,
        }),
        _ => None,
    });
    let seasons = series
        .seasons
        .into_iter()
        .map(|s| {
            let total = s
                .statistics
                .as_ref()
                .map(|s| s.total_episode_count.max(s.episode_count));
            let imported = s.statistics.as_ref().map(|s| s.episode_file_count);
            Season {
                number: s.season_number,
                episode_count: total,
                imported_count: imported,
                active_count: None,
                monitored: s.monitored,
                state: match (total, imported) {
                    (Some(t), Some(i)) => season_state(t, i, 0),
                    _ => SeasonState::Unknown,
                },
            }
        })
        .collect();
    Ok(MediaItem::Series {
        tvdb_id: id,
        title: safe_text(&series.title, 200),
        year: (series.year > 0).then_some(series.year),
        overview: safe_text(&series.overview, 2000),
        poster: (!series.images.is_empty()).then(|| format!("api/v1/artwork/series/{id}/poster")),
        default_profile,
        library,
        acquisition: if series.id == 0 {
            Acquisition::NotRequested
        } else {
            Acquisition::Unknown
        },
        seasons,
    })
}
fn season_state(total: u32, imported: u32, active: u32) -> SeasonState {
    if active > 0 {
        SeasonState::InProgress
    } else if total > 0 && imported >= total {
        SeasonState::Imported
    } else if imported > 0 {
        SeasonState::Partial
    } else {
        SeasonState::Missing
    }
}
fn queue_acquisition(queue: &UpQueue) -> Acquisition {
    match (
        queue.tracked_download_status.as_str(),
        queue.tracked_download_state.as_str(),
        queue.status.as_str(),
    ) {
        ("error", _, _) => Acquisition::Blocked {
            reason: AcquisitionBlock::DownloadFailed,
        },
        (_, "importPending" | "importing", _) => Acquisition::Importing,
        (_, "importFailed", _) => Acquisition::Blocked {
            reason: AcquisitionBlock::ImportFailed,
        },
        (_, _, "completed") => Acquisition::Importing,
        (_, _, "downloading")
            if queue.size.is_finite()
                && queue.sizeleft.is_finite()
                && queue.size > 0.0
                && queue.sizeleft >= 0.0
                && queue.sizeleft <= queue.size =>
        {
            Acquisition::Downloading {
                completed_bytes: (queue.size - queue.sizeleft) as u64,
                total_bytes: queue.size as u64,
            }
        }
        _ => Acquisition::Waiting,
    }
}

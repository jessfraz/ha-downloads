//! A durable, checkpointed request worker. An ambiguous upstream write is read
//! back, never blindly replayed after a timeout or process restart.
use crate::media::{
    model::*,
    service::{MediaService, digest, fingerprint, hash, page},
    store::{Checkpoint, Plan, StoredRequest},
    upstream::*,
};
use reqwest::Method;
use std::time::Duration;
use tokio::sync::watch;

impl MediaService {
    pub async fn create_request(
        &self,
        actor: &str,
        key: &str,
        input: CreateRequest,
    ) -> Result<RequestRecord, ApiErrorCode> {
        let _guard = self.operations.lock().await;
        let key_hash = hash(&[key]);
        let fingerprint = fingerprint(&input)?;
        let snapshot = self.store.snapshot().await;
        if let Some(existing) = snapshot
            .requests
            .iter()
            .find(|r| r.actor == actor && r.key_hash == key_hash)
        {
            return if existing.fingerprint == fingerprint {
                Ok(existing.record.clone())
            } else {
                Err(ApiErrorCode::IdempotencyConflict)
            };
        }
        let now = crate::monitor::unix_ms();
        let as_of = utc_timestamp(now)?;
        let plan = match &input {
            CreateRequest::Movie { tmdb_id, intent } => {
                let media = self.find_movie(*tmdb_id).await?;
                let choices = self.native_choices(ManagerId::Radarr).await?;
                let (settings, root) = choices.movie_settings(&media)?;
                if media.id > 0 && matches!(intent, MovieIntent::MonitorOnly {}) && !media.monitored
                {
                    return Err(ApiErrorCode::NativeSettingsRequired);
                }
                Plan::Movie {
                    existing_id: (media.id > 0).then_some(media.id),
                    media,
                    settings,
                    root,
                    search: matches!(intent, MovieIntent::SearchNow {}),
                }
            }
            CreateRequest::Series { tvdb_id, intent } => {
                let media = self.find_series(*tvdb_id).await?;
                let choices = self.native_choices(ManagerId::Sonarr).await?;
                let (settings, root) = choices.series_settings(&media, intent)?;
                let selected_seasons = match intent {
                    SeriesIntent::SearchNow {
                        scope: EpisodeScope::Seasons { numbers },
                    } => {
                        if numbers.numbers().iter().any(|number| {
                            !media
                                .seasons
                                .iter()
                                .any(|season| season.season_number == *number)
                        }) {
                            return Err(ApiErrorCode::InvalidScope);
                        }
                        numbers.numbers().to_vec()
                    }
                    SeriesIntent::SearchNow {
                        scope: EpisodeScope::AllAired {},
                    } => media
                        .seasons
                        .iter()
                        .filter(|season| season.season_number > 0)
                        .map(|season| season.season_number)
                        .collect(),
                    SeriesIntent::MonitorOnly {} => {
                        if settings.future_monitoring == FutureMonitoring::Off
                            || (media.id > 0 && !media.monitored)
                        {
                            return Err(ApiErrorCode::NativeSettingsRequired);
                        }
                        vec![]
                    }
                };
                if matches!(intent, SeriesIntent::SearchNow { .. }) && selected_seasons.is_empty() {
                    return Err(ApiErrorCode::InvalidScope);
                }
                Plan::Series {
                    existing_id: (media.id > 0).then_some(media.id),
                    media,
                    settings,
                    root,
                    search: matches!(intent, SeriesIntent::SearchNow { .. }),
                    selected_seasons,
                    as_of,
                }
            }
        };
        let record = RequestRecord {
            id: RequestId::from_digest(digest(&["request", actor, key])),
            created_at_unix_ms: now,
            input,
            submission: Submission::Pending,
        };
        if !self.accepting() {
            return Err(ApiErrorCode::UpstreamUnavailable);
        }
        self.store
            .update(|state| {
                state.requests.push(StoredRequest {
                    actor: actor.into(),
                    key_hash,
                    fingerprint,
                    record: record.clone(),
                    plan,
                    checkpoint: Checkpoint::Prepared,
                });
                Ok(record)
            })
            .await
    }
    pub async fn request(
        &self,
        actor: &str,
        admin: bool,
        id: &RequestId,
    ) -> Result<RequestRecord, ApiErrorCode> {
        self.store
            .snapshot()
            .await
            .requests
            .into_iter()
            .find(|r| &r.record.id == id && (r.actor == actor || admin))
            .map(|r| r.record)
            .ok_or(ApiErrorCode::NotFound)
    }
    pub async fn request_by_key(
        &self,
        actor: &str,
        key: &str,
    ) -> Result<RequestRecord, ApiErrorCode> {
        let key_hash = hash(&[key]);
        self.store
            .snapshot()
            .await
            .requests
            .into_iter()
            .find(|r| r.actor == actor && r.key_hash == key_hash)
            .map(|r| r.record)
            .ok_or(ApiErrorCode::NotFound)
    }
    pub async fn requests(
        &self,
        actor: &str,
        admin: bool,
        limit: Option<u32>,
        token: Option<&str>,
    ) -> Result<Page<RequestRecord>, ApiErrorCode> {
        let items = self
            .store
            .snapshot()
            .await
            .requests
            .into_iter()
            .filter(|r| r.actor == actor || admin)
            .map(|r| r.record)
            .collect();
        page(
            items,
            limit,
            token,
            &format!("requests:{}:{admin}", hash(&[actor])),
            |r| r.id.to_string(),
            vec![],
        )
    }
    pub(crate) async fn request_loop(&self, mut stop: watch::Receiver<bool>) {
        let mut tick = tokio::time::interval(Duration::from_secs(3));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut position = 0usize;
        loop {
            tokio::select! {_=stop.changed()=>return,_=tick.tick()=>{
                let pending=self.store.snapshot().await.requests.into_iter().filter(|r|!matches!(r.checkpoint,Checkpoint::Complete)).collect::<Vec<_>>();
                if pending.is_empty(){continue}
                let Some(request)=pending.get(position%pending.len()).cloned()else{continue};position=position.wrapping_add(1);
                tokio::select!{_=stop.changed()=>return,_=self.process_request(request)=>{}}
            }}
        }
    }
    async fn process_request(&self, mut request: StoredRequest) {
        let _guard = self.operations.lock().await;
        let outcome = self.advance_request(&mut request).await;
        if let Err(code) = outcome {
            let uncertain = matches!(
                code,
                ApiErrorCode::UpstreamUnavailable
                    | ApiErrorCode::InvalidUpstreamResponse
                    | ApiErrorCode::NotConfigured
            );
            if uncertain {
                if !matches!(request.record.submission, Submission::Reconciling { .. }) {
                    request.record.submission = Submission::Reconciling {
                        reason: ReconciliationReason::UpstreamOutcomeUnknown,
                    };
                    let _ = self.store.save_request(request).await;
                }
            } else {
                request.record.submission = Submission::Failed {
                    error: RequestFailure {
                        code,
                        message: request_failure(code).into(),
                    },
                };
                request.checkpoint = Checkpoint::Complete;
                let _ = self.store.save_request(request).await;
            }
        }
    }
    async fn save_checkpoint(&self, request: &mut StoredRequest) -> Result<(), ApiErrorCode> {
        if let Err(error) = self.store.save_request(request.clone()).await {
            // An uncommitted transition must not be persisted by the error
            // handler as though its following upstream effect had happened.
            if let Some(committed) = self
                .store
                .snapshot()
                .await
                .requests
                .into_iter()
                .find(|item| item.record.id == request.record.id)
            {
                *request = committed;
            }
            return Err(error);
        }
        Ok(())
    }
    async fn advance_request(&self, request: &mut StoredRequest) -> Result<(), ApiErrorCode> {
        if matches!(request.checkpoint, Checkpoint::Prepared) {
            request.record.submission = Submission::Applying;
            let existing_id = match &request.plan {
                Plan::Movie { existing_id, .. } | Plan::Series { existing_id, .. } => *existing_id,
            };
            if let Some(upstream_id) = existing_id {
                request.checkpoint = Checkpoint::Added {
                    upstream_id,
                    effect: RequestEffect::AlreadyManaged,
                };
                self.save_checkpoint(request).await?;
            } else {
                request.checkpoint = Checkpoint::Adding;
                self.save_checkpoint(request).await?;
                let upstream_id = match &request.plan {
                    Plan::Movie {
                        media,
                        settings,
                        root,
                        ..
                    } => {
                        let minimum = match settings.minimum_availability {
                            MinimumAvailability::Announced => "announced",
                            MinimumAvailability::InCinemas => "inCinemas",
                            MinimumAvailability::Released => "released",
                        };
                        let added = self
                            .radarr
                            .write::<_, UpMovie>(
                                Method::POST,
                                "movie",
                                &AddMovie {
                                    title: &media.title,
                                    title_slug: &media.title_slug,
                                    year: media.year,
                                    tmdb_id: media.tmdb_id,
                                    quality_profile_id: settings.quality_profile_id.get(),
                                    root_folder_path: root,
                                    monitored: true,
                                    minimum_availability: minimum,
                                    tags: settings.tags.iter().map(|t| t.get()).collect(),
                                    images: &media.images,
                                    add_options: MovieAddOptions {
                                        search_for_movie: false,
                                    },
                                },
                            )
                            .await?;
                        if added.tmdb_id != media.tmdb_id || added.id == 0 {
                            return Err(ApiErrorCode::InvalidUpstreamResponse);
                        }
                        added.id
                    }
                    Plan::Series {
                        media,
                        settings,
                        root,
                        ..
                    } => {
                        let future = settings.future_monitoring == FutureMonitoring::Monitor;
                        // Sonarr's "skip" preserves the explicit season flags; "future"
                        // enables only future episodes when that saved policy opted in.
                        let seasons = media
                            .seasons
                            .iter()
                            .map(|s| UpSeason {
                                season_number: s.season_number,
                                monitored: false,
                                statistics: None,
                            })
                            .collect();
                        let series_type = settings.series_type.to_string();
                        let added = self
                            .sonarr
                            .write::<_, UpSeries>(
                                Method::POST,
                                "series",
                                &AddSeries {
                                    title: &media.title,
                                    title_slug: &media.title_slug,
                                    year: media.year,
                                    tvdb_id: media.tvdb_id,
                                    quality_profile_id: settings.quality_profile_id.get(),
                                    root_folder_path: root,
                                    monitored: true,
                                    season_folder: settings.season_folders,
                                    series_type: &series_type,
                                    monitor_new_items: if future { "all" } else { "none" },
                                    seasons,
                                    tags: settings.tags.iter().map(|t| t.get()).collect(),
                                    images: &media.images,
                                    add_options: SeriesAddOptions {
                                        monitor: if future { "future" } else { "skip" },
                                        search_for_missing_episodes: false,
                                        search_for_cutoff_unmet_episodes: false,
                                    },
                                },
                            )
                            .await?;
                        if added.tvdb_id != media.tvdb_id || added.id == 0 {
                            return Err(ApiErrorCode::InvalidUpstreamResponse);
                        }
                        added.id
                    }
                };
                request.checkpoint = Checkpoint::Added {
                    upstream_id,
                    effect: RequestEffect::Added,
                };
                self.save_checkpoint(request).await?;
            }
        }
        if matches!(request.checkpoint, Checkpoint::Adding) {
            // This is a recovered/uncertain add. Canonical identity proves whether
            // it exists; absence is not permission to repeat the original POST.
            let upstream_id = match &request.plan {
                Plan::Movie { media, .. } => {
                    self.find_movie(
                        TmdbId::try_from(media.tmdb_id)
                            .map_err(|_| ApiErrorCode::InvalidUpstreamResponse)?,
                    )
                    .await?
                    .id
                }
                Plan::Series { media, .. } => {
                    self.find_series(
                        TvdbId::try_from(media.tvdb_id)
                            .map_err(|_| ApiErrorCode::InvalidUpstreamResponse)?,
                    )
                    .await?
                    .id
                }
            };
            if upstream_id == 0 {
                return Err(ApiErrorCode::UpstreamUnavailable);
            }
            request.checkpoint = Checkpoint::Added {
                upstream_id,
                effect: RequestEffect::Added,
            };
            self.save_checkpoint(request).await?;
        }
        if let Checkpoint::Added {
            upstream_id,
            effect,
        } = request.checkpoint.clone()
        {
            match &request.plan {
                Plan::Movie {
                    media,
                    settings,
                    root,
                    search,
                    ..
                } => {
                    let current = self
                        .find_movie(
                            TmdbId::try_from(media.tmdb_id)
                                .map_err(|_| ApiErrorCode::InvalidUpstreamResponse)?,
                        )
                        .await?;
                    if current.id != upstream_id
                        || current.quality_profile_id != settings.quality_profile_id.get()
                        || !matches_frozen_root(&current.root_folder_path, &current.path, root)
                    {
                        return Err(ApiErrorCode::ExistingSettingsConflict);
                    }
                    if current.has_file || current.movie_file_id > 0 {
                        return self
                            .finish(
                                request,
                                upstream_id,
                                RequestEffect::AlreadySatisfied,
                                SearchDispatch::NotRequested,
                            )
                            .await;
                    }
                    if !search {
                        return self
                            .finish(request, upstream_id, effect, SearchDispatch::NotRequested)
                            .await;
                    }
                    let command = Command::MoviesSearch {
                        movie_ids: vec![upstream_id],
                    };
                    self.submit_search(request, upstream_id, effect, command)
                        .await?;
                }
                Plan::Series {
                    media,
                    settings,
                    root,
                    search,
                    selected_seasons,
                    as_of,
                    ..
                } => {
                    let current = self
                        .find_series(
                            TvdbId::try_from(media.tvdb_id)
                                .map_err(|_| ApiErrorCode::InvalidUpstreamResponse)?,
                        )
                        .await?;
                    if current.id != upstream_id
                        || current.quality_profile_id != settings.quality_profile_id.get()
                        || !matches_frozen_root(&current.root_folder_path, &current.path, root)
                    {
                        return Err(ApiErrorCode::ExistingSettingsConflict);
                    }
                    if !search {
                        return self
                            .finish(request, upstream_id, effect, SearchDispatch::NotRequested)
                            .await;
                    }
                    let episodes = self
                        .sonarr
                        .get::<Vec<UpEpisode>>("episode", &[("seriesId", upstream_id.to_string())])
                        .await?;
                    if episodes.is_empty() {
                        return Err(ApiErrorCode::UpstreamUnavailable);
                    }
                    let selected = episodes
                        .iter()
                        .filter(|e| selected_seasons.contains(&e.season_number) && aired(e, as_of))
                        .collect::<Vec<_>>();
                    if selected.is_empty() {
                        if settings.future_monitoring == FutureMonitoring::Monitor {
                            return self
                                .finish(request, upstream_id, effect, SearchDispatch::NotRequested)
                                .await;
                        }
                        return Err(ApiErrorCode::InvalidScope);
                    }
                    let episode_ids = selected
                        .iter()
                        .filter(|e| !e.has_file)
                        .map(|e| e.id)
                        .collect::<Vec<_>>();
                    if episode_ids.is_empty() {
                        return self
                            .finish(
                                request,
                                upstream_id,
                                RequestEffect::AlreadySatisfied,
                                SearchDispatch::NotRequested,
                            )
                            .await;
                    }
                    if episode_ids.len() > 5000 || episode_ids.contains(&0) {
                        return Err(ApiErrorCode::InvalidScope);
                    }
                    if selected.iter().filter(|e| !e.has_file).all(|e| e.monitored) {
                        request.checkpoint = Checkpoint::Monitored {
                            upstream_id,
                            effect,
                            episode_ids,
                        };
                        self.save_checkpoint(request).await?;
                    } else {
                        request.checkpoint = Checkpoint::Monitoring {
                            upstream_id,
                            effect,
                            episode_ids: episode_ids.clone(),
                        };
                        self.save_checkpoint(request).await?;
                        // Only the frozen missing, aired episode IDs are changed.
                        // No full Series PUT can clobber unrelated native settings.
                        self.sonarr
                            .write_empty(
                                Method::PUT,
                                "episode/monitor",
                                &MonitorEpisodes {
                                    episode_ids,
                                    monitored: true,
                                },
                            )
                            .await?;
                    }
                }
            }
        }
        if let Checkpoint::Monitoring {
            upstream_id,
            effect,
            episode_ids,
        } = request.checkpoint.clone()
        {
            let episodes = self
                .sonarr
                .get::<Vec<UpEpisode>>("episode", &[("seriesId", upstream_id.to_string())])
                .await?;
            if !episode_ids.iter().all(|id| {
                episodes
                    .iter()
                    .any(|e| e.id == *id && (e.has_file || e.monitored))
            }) {
                return Err(ApiErrorCode::UpstreamUnavailable);
            }
            request.checkpoint = Checkpoint::Monitored {
                upstream_id,
                effect,
                episode_ids,
            };
            self.save_checkpoint(request).await?;
        }
        if let Checkpoint::Monitored {
            upstream_id,
            effect,
            episode_ids,
        } = request.checkpoint.clone()
        {
            // Recheck the frozen selection immediately before dispatch. A file
            // can arrive while monitoring is being applied or after a restart;
            // that does not authorize a fresh search for an imported episode.
            let current = self
                .sonarr
                .get::<Vec<UpEpisode>>("episode", &[("seriesId", upstream_id.to_string())])
                .await?;
            let mut missing = vec![];
            for id in episode_ids {
                let episode = current
                    .iter()
                    .find(|episode| episode.id == id)
                    .ok_or(ApiErrorCode::ExistingSettingsConflict)?;
                if episode.has_file {
                    continue;
                }
                if !episode.monitored {
                    return Err(ApiErrorCode::ExistingSettingsConflict);
                }
                missing.push(id);
            }
            if missing.is_empty() {
                return self
                    .finish(
                        request,
                        upstream_id,
                        RequestEffect::AlreadySatisfied,
                        SearchDispatch::NotRequested,
                    )
                    .await;
            }
            self.submit_search(
                request,
                upstream_id,
                effect,
                Command::EpisodeSearch {
                    episode_ids: missing,
                },
            )
            .await?;
        }
        if let Checkpoint::Searching {
            upstream_id,
            effect,
            command,
        } = request.checkpoint.clone()
        {
            let manager = match request.plan {
                Plan::Movie { .. } => ManagerId::Radarr,
                Plan::Series { .. } => ManagerId::Sonarr,
            };
            let cutoff = utc_timestamp(request.record.created_at_unix_ms)?;
            let commands = self
                .manager(manager)
                .get::<Vec<UpCommand>>("command", &[])
                .await?;
            if let Some(found) = commands.into_iter().find(|c| {
                c.id > 0
                    && c.body.as_ref() == Some(&command)
                    && c.queued
                        .as_deref()
                        .is_some_and(|at| at.get(..19).is_some_and(|at| at >= &cutoff[..19]))
            }) {
                return self
                    .finish(
                        request,
                        upstream_id,
                        effect,
                        SearchDispatch::Queued {
                            command_id: found.id,
                        },
                    )
                    .await;
            }
            return Err(ApiErrorCode::UpstreamUnavailable);
        }
        Ok(())
    }
    async fn submit_search(
        &self,
        request: &mut StoredRequest,
        upstream_id: u32,
        effect: RequestEffect,
        command: Command,
    ) -> Result<(), ApiErrorCode> {
        request.checkpoint = Checkpoint::Searching {
            upstream_id,
            effect,
            command: command.clone(),
        };
        self.save_checkpoint(request).await?;
        let manager = match request.plan {
            Plan::Movie { .. } => ManagerId::Radarr,
            Plan::Series { .. } => ManagerId::Sonarr,
        };
        let queued = self
            .manager(manager)
            .write::<_, UpCommand>(Method::POST, "command", &command)
            .await?;
        if queued.id == 0 {
            return Err(ApiErrorCode::InvalidUpstreamResponse);
        }
        let verified = self
            .manager(manager)
            .get::<UpCommand>(&format!("command/{}", queued.id), &[])
            .await?;
        if verified.id != queued.id || verified.body.as_ref() != Some(&command) {
            return Err(ApiErrorCode::InvalidUpstreamResponse);
        }
        self.finish(
            request,
            upstream_id,
            effect,
            SearchDispatch::Queued {
                command_id: verified.id,
            },
        )
        .await
    }
    async fn finish(
        &self,
        request: &mut StoredRequest,
        upstream_id: u32,
        effect: RequestEffect,
        search: SearchDispatch,
    ) -> Result<(), ApiErrorCode> {
        let media = request.record.input.identity();
        let manager = match media {
            MediaIdentity::Movie { .. } => ManagerId::Radarr,
            MediaIdentity::Series { .. } => ManagerId::Sonarr,
        };
        let mut completed = request.clone();
        completed.record.submission = Submission::Applied {
            result: AppliedResult {
                effect,
                media,
                manager,
                upstream_id,
                search,
            },
        };
        completed.checkpoint = Checkpoint::Complete;
        self.save_checkpoint(&mut completed).await?;
        *request = completed;
        Ok(())
    }
}
fn request_failure(code: ApiErrorCode) -> &'static str {
    match code {
        ApiErrorCode::ExistingSettingsConflict => {
            "The existing title has different settings. Nothing was overwritten."
        }
        ApiErrorCode::InvalidScope => {
            "The selected scope has no eligible aired episodes. Review monitoring and season selection."
        }
        _ => "The request could not be applied. Inspect its current state before trying again.",
    }
}
fn matches_frozen_root(root_path: &str, media_path: &str, frozen_root: &str) -> bool {
    if !root_path.is_empty() {
        return root_path == frozen_root;
    }
    // Native library responses can omit rootFolderPath. Keep the accepted
    // destination proof, using the same path-component boundary as admission.
    !frozen_root.is_empty()
        && (media_path == frozen_root
            || media_path
                .strip_prefix(frozen_root.trim_end_matches('/'))
                .is_some_and(|suffix| suffix.starts_with('/')))
}
fn aired(episode: &UpEpisode, cutoff: &str) -> bool {
    episode
        .air_date_utc
        .as_deref()
        .and_then(utc_instant)
        .zip(utc_instant(cutoff))
        .is_some_and(|(at, cutoff)| at <= cutoff)
}
// Native timestamps must be real UTC instants. Unknown/malformed airtimes are
// not eligible for an aired-only request, including offset or date-only input.
fn utc_instant(value: &str) -> Option<(u16, u8, u8, u8, u8, u8, u32)> {
    let bytes = value.as_bytes();
    if bytes.len() < 20
        || bytes.get(4) != Some(&b'-')
        || bytes.get(7) != Some(&b'-')
        || bytes.get(10) != Some(&b'T')
        || bytes.get(13) != Some(&b':')
        || bytes.get(16) != Some(&b':')
    {
        return None;
    }
    let year = value.get(..4)?.parse::<u16>().ok()?;
    let month = value.get(5..7)?.parse::<u8>().ok()?;
    let day = value.get(8..10)?.parse::<u8>().ok()?;
    let hour = value.get(11..13)?.parse::<u8>().ok()?;
    let minute = value.get(14..16)?.parse::<u8>().ok()?;
    let second = value.get(17..19)?.parse::<u8>().ok()?;
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if leap {
                29
            } else {
                28
            }
        }
        _ => return None,
    };
    if year == 0 || day == 0 || day > days || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let tail = value.get(19..)?;
    let nanos = if matches!(tail, "Z" | "+00:00") {
        0
    } else {
        let fractional = tail.strip_prefix('.')?;
        let digits = fractional
            .strip_suffix('Z')
            .or_else(|| fractional.strip_suffix("+00:00"))?;
        if digits.is_empty() || digits.len() > 9 || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        digits.parse::<u32>().ok()? * 10u32.pow(9 - digits.len() as u32)
    };
    Some((year, month, day, hour, minute, second, nanos))
}
pub(crate) fn utc_timestamp(ms: u64) -> Result<String, ApiErrorCode> {
    let seconds = (ms / 1000)
        .try_into()
        .map_err(|_| ApiErrorCode::InvalidArguments)?;
    let mut value = std::mem::MaybeUninit::<libc::tm>::uninit();
    // SAFETY: gmtime_r writes a tm to the valid out pointer, and its return is
    // checked before reading it. The source time_t outlives the call.
    if unsafe { libc::gmtime_r(&seconds, value.as_mut_ptr()) }.is_null() {
        return Err(ApiErrorCode::InvalidArguments);
    }
    // SAFETY: successful gmtime_r initialized every field.
    let value = unsafe { value.assume_init() };
    Ok(format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        value.tm_year + 1900,
        value.tm_mon + 1,
        value.tm_mday,
        value.tm_hour,
        value.tm_min,
        value.tm_sec,
        ms % 1000
    ))
}

#[cfg(test)]
mod tests {
    use crate::{
        config::MediaConfig,
        media::{
            model::*,
            service::MediaService,
            store::{Checkpoint, Plan, StoredRequest},
            upstream::UpMovie,
        },
        status::Status,
    };
    use tokio::sync::watch;

    #[tokio::test]
    async fn failed_final_persistence_retains_recoverable_checkpoint() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("media");
        let (_tx, rx) = watch::channel(Status::needs_configuration("fixture"));
        let service = MediaService::open(MediaConfig::default(), directory.clone(), rx, vec![])
            .await
            .unwrap();
        let settings = MovieSettings {
            quality_profile_id: QualityProfileId::try_from(1).unwrap(),
            root_folder_id: RootFolderId::try_from(1).unwrap(),
            tags: vec![],
            minimum_availability: MinimumAvailability::Released,
        };
        let input = CreateRequest::Movie {
            tmdb_id: TmdbId::try_from(14337).unwrap(),
            intent: MovieIntent::MonitorOnly {},
        };
        let media = UpMovie {
            id: 7,
            tmdb_id: 14337,
            title: "Primer".into(),
            title_slug: String::new(),
            year: 2004,
            overview: String::new(),
            images: vec![],
            quality_profile_id: 1,
            root_folder_path: "/media/movies".into(),
            minimum_availability: "released".into(),
            path: String::new(),
            monitored: true,
            has_file: false,
            movie_file_id: 0,
            tags: vec![],
        };
        let mut request = StoredRequest {
            actor: "fixture".into(),
            key_hash: "fixture".into(),
            fingerprint: "fixture".into(),
            record: RequestRecord {
                id: "2db98f62-f0f0-4c4b-b79b-a36eb2f90810".parse().unwrap(),
                created_at_unix_ms: 0,
                input,
                submission: Submission::Applying,
            },
            plan: Plan::Movie {
                media,
                settings,
                root: "/media/movies".into(),
                search: false,
                existing_id: Some(7),
            },
            checkpoint: Checkpoint::Added {
                upstream_id: 7,
                effect: RequestEffect::AlreadyManaged,
            },
        };
        service
            .store
            .update(|state| {
                state.requests.push(request.clone());
                Ok(())
            })
            .await
            .unwrap();
        tokio::fs::create_dir(directory.join(".journal.tmp"))
            .await
            .unwrap();
        assert!(
            service
                .finish(
                    &mut request,
                    7,
                    RequestEffect::AlreadyManaged,
                    SearchDispatch::NotRequested
                )
                .await
                .is_err()
        );
        assert!(matches!(request.checkpoint, Checkpoint::Added { .. }));
        assert!(matches!(
            service.store.snapshot().await.requests[0].checkpoint,
            Checkpoint::Added { .. }
        ));
        tokio::fs::remove_dir(directory.join(".journal.tmp"))
            .await
            .unwrap();
        service
            .finish(
                &mut request,
                7,
                RequestEffect::AlreadyManaged,
                SearchDispatch::NotRequested,
            )
            .await
            .unwrap();
        assert!(matches!(request.checkpoint, Checkpoint::Complete));
        assert!(matches!(
            service.store.snapshot().await.requests[0].record.submission,
            Submission::Applied { .. }
        ));
    }
}

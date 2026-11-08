//! Read the managers' current configuration. Downloads owns no settings profile.
use crate::media::{
    model::*,
    service::{MediaService, RootObservation},
    store::{NativeBinding, NativeDefaults},
    upstream::{UpMovie, UpNamed, UpRoot, UpSeries},
};

pub(crate) struct NativeChoices {
    quality_profiles: Vec<UpNamed>,
    roots: Vec<UpRoot>,
    defaults: NativeDefaults,
}
impl MediaService {
    pub(crate) async fn native_choices(
        &self,
        manager: ManagerId,
    ) -> Result<NativeChoices, ApiErrorCode> {
        let upstream = self.manager(manager);
        let (quality_profiles, roots) = tokio::try_join!(
            upstream.get::<Vec<UpNamed>>("qualityprofile", &[]),
            upstream.get::<Vec<UpRoot>>("rootfolder", &[])
        )?;
        if quality_profiles.len() > 200
            || roots.len() > 100
            || quality_profiles.iter().enumerate().any(|(index, profile)| {
                QualityProfileId::try_from(profile.id).is_err()
                    || quality_profiles[..index]
                        .iter()
                        .any(|other| other.id == profile.id)
            })
            || roots.iter().enumerate().any(|(index, root)| {
                RootFolderId::try_from(root.id).is_err()
                    || root.path.is_empty()
                    || roots[..index].iter().any(|other| other.id == root.id)
            })
        {
            return Err(ApiErrorCode::InvalidUpstreamResponse);
        }
        self.cache.write().await.roots.insert(
            manager,
            RootObservation {
                accessible: roots.iter().any(root_accessible),
                observed_at_unix_ms: crate::monitor::unix_ms(),
            },
        );
        Ok(NativeChoices {
            quality_profiles,
            roots,
            defaults: self.store.native_defaults().await,
        })
    }
}
impl NativeChoices {
    pub(crate) fn movie_availability(&self, media: &UpMovie) -> RequestAvailability {
        availability(self.movie_settings(media).is_ok())
    }
    pub(crate) fn series_availability(&self, media: &UpSeries) -> RequestAvailability {
        // Eligibility concerns the native destination and quality. A request's
        // explicit monitoring/search intent is validated at admission.
        availability(
            self.selection(
                media.id,
                media.quality_profile_id,
                &media.root_folder_path,
                &media.path,
                self.defaults.series.as_ref(),
            )
            .is_ok(),
        )
    }
    fn selection(
        &self,
        native_id: u32,
        quality: u32,
        root_path: &str,
        media_path: &str,
        binding: Option<&NativeBinding>,
    ) -> Result<(QualityProfileId, &UpRoot), ApiErrorCode> {
        if native_id == 0 {
            // These are private references to previously configured native
            // objects, never a second copy of the managers' quality rules.
            let (quality, root) = if let Some(binding) = binding {
                let quality = self
                    .quality_profiles
                    .iter()
                    .find(|quality| quality.id == binding.quality_profile_id.get())
                    .ok_or(ApiErrorCode::NativeSettingsRequired)?;
                let root = self
                    .roots
                    .iter()
                    .find(|root| root.id == binding.root_folder_id.get())
                    .ok_or(ApiErrorCode::NativeSettingsRequired)?;
                (quality, root)
            } else {
                let [quality] = self.quality_profiles.as_slice() else {
                    return Err(ApiErrorCode::NativeSettingsRequired);
                };
                let [root] = self.roots.as_slice() else {
                    return Err(ApiErrorCode::NativeSettingsRequired);
                };
                (quality, root)
            };
            if !root_accessible(root) {
                return Err(ApiErrorCode::NativeSettingsRequired);
            }
            return Ok((
                QualityProfileId::try_from(quality.id)
                    .map_err(|_| ApiErrorCode::InvalidUpstreamResponse)?,
                root,
            ));
        }
        let quality = QualityProfileId::try_from(quality)
            .map_err(|_| ApiErrorCode::NativeSettingsRequired)?;
        if !self
            .quality_profiles
            .iter()
            .any(|profile| profile.id == quality.get())
        {
            return Err(ApiErrorCode::NativeSettingsRequired);
        }
        let mut matching = self
            .roots
            .iter()
            .filter(|root| {
                if !root_path.is_empty() {
                    root.path == root_path
                } else {
                    let prefix = format!("{}/", root.path.trim_end_matches('/'));
                    media_path == root.path || media_path.starts_with(&prefix)
                }
            })
            .collect::<Vec<_>>();
        matching.sort_by_key(|root| std::cmp::Reverse(root.path.len()));
        let root = matching
            .first()
            .copied()
            .ok_or(ApiErrorCode::NativeSettingsRequired)?;
        if matching
            .get(1)
            .is_some_and(|other| other.path.len() == root.path.len())
            || !root_accessible(root)
        {
            return Err(ApiErrorCode::NativeSettingsRequired);
        }
        Ok((quality, root))
    }
    pub(crate) fn movie_settings(
        &self,
        media: &UpMovie,
    ) -> Result<(MovieSettings, String), ApiErrorCode> {
        let (quality_profile_id, root) = self.selection(
            media.id,
            media.quality_profile_id,
            &media.root_folder_path,
            &media.path,
            self.defaults.movie.as_ref(),
        )?;
        let minimum_availability = match media.minimum_availability.as_str() {
            "announced" => MinimumAvailability::Announced,
            "inCinemas" | "in_cinemas" => MinimumAvailability::InCinemas,
            "released" | "" => MinimumAvailability::Released,
            _ => return Err(ApiErrorCode::NativeSettingsRequired),
        };
        Ok((
            MovieSettings {
                quality_profile_id,
                root_folder_id: RootFolderId::try_from(root.id)
                    .map_err(|_| ApiErrorCode::InvalidUpstreamResponse)?,
                tags: tags(&media.tags)?,
                minimum_availability,
            },
            root.path.clone(),
        ))
    }
    pub(crate) fn series_settings(
        &self,
        media: &UpSeries,
        intent: &SeriesIntent,
    ) -> Result<(SeriesSettings, String), ApiErrorCode> {
        let (quality_profile_id, root) = self.selection(
            media.id,
            media.quality_profile_id,
            &media.root_folder_path,
            &media.path,
            self.defaults.series.as_ref(),
        )?;
        let series_type = match media.series_type.as_str() {
            "standard" | "" => SeriesType::Standard,
            "daily" => SeriesType::Daily,
            "anime" => SeriesType::Anime,
            _ => return Err(ApiErrorCode::NativeSettingsRequired),
        };
        let future_monitoring = if media.id == 0 {
            // Search scope never opts an unconfigured series into future episodes.
            // Monitor-only is an explicit request to monitor without a search.
            if matches!(intent, SeriesIntent::MonitorOnly {}) {
                FutureMonitoring::Monitor
            } else {
                FutureMonitoring::Off
            }
        } else {
            match media.monitor_new_items.as_str() {
                "all" => FutureMonitoring::Monitor,
                "none" | "" => FutureMonitoring::Off,
                _ => return Err(ApiErrorCode::NativeSettingsRequired),
            }
        };
        Ok((
            SeriesSettings {
                quality_profile_id,
                root_folder_id: RootFolderId::try_from(root.id)
                    .map_err(|_| ApiErrorCode::InvalidUpstreamResponse)?,
                tags: tags(&media.tags)?,
                series_type,
                season_folders: media.season_folder,
                future_monitoring,
            },
            root.path.clone(),
        ))
    }
}
fn availability(ready: bool) -> RequestAvailability {
    if ready {
        RequestAvailability::Ready
    } else {
        RequestAvailability::ParentSetupRequired
    }
}
fn root_accessible(root: &UpRoot) -> bool {
    root.accessible && root.free_space != Some(0)
}
fn tags(values: &[u32]) -> Result<Vec<TagId>, ApiErrorCode> {
    if values.len() > 100 {
        return Err(ApiErrorCode::InvalidUpstreamResponse);
    }
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            if values[..index].contains(value) {
                return Err(ApiErrorCode::InvalidUpstreamResponse);
            }
            TagId::try_from(*value).map_err(|_| ApiErrorCode::InvalidUpstreamResponse)
        })
        .collect()
}

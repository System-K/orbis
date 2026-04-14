// =============================================================================
// Orbis — Background Download Manager
// =============================================================================
// Manages concurrent layer downloads from various providers.
// Each download runs in its own thread and sends results via mpsc channel.
// =============================================================================

use std::path::PathBuf;
use std::sync::mpsc;

use crate::provider::{LayerImage, ProviderCatalog};

/// A completed download result from a background thread.
pub(crate) struct DownloadResult {
    pub provider_id: String,
    pub label: String,
    pub opacity: f32,
    pub enabled: bool,
    pub result: Result<LayerImage, String>,
}

/// Manages pending download receivers.
pub(crate) struct DownloadManager {
    pending: Vec<mpsc::Receiver<DownloadResult>>,
}

impl DownloadManager {
    pub fn new() -> Self {
        Self {
            pending: Vec::new(),
        }
    }

    /// Starts a download for a provider in a background thread.
    pub fn start_download(
        &mut self,
        catalog: &ProviderCatalog,
        provider_id: &str,
        date: Option<chrono::NaiveDate>,
        opacity: f32,
        enabled: bool,
    ) {
        let provider = match catalog.find(provider_id) {
            Some(p) => p,
            None => {
                log::warn!("Provider not found: '{}'", provider_id);
                return;
            }
        };

        let info = provider.info().clone();
        let pid = provider_id.to_string();
        let (tx, rx) = mpsc::channel();

        let cache_dir_path = if pid.starts_with("gibs_") || pid == "builtin:grid" {
            PathBuf::from("cache/gibs")
        } else {
            PathBuf::from("cache/wms")
        };
        let provider_id_clone = pid.clone();
        let label = info.label.clone();

        std::thread::spawn(move || {
            let mut providers = crate::gibs::all_gibs_providers();
            providers.extend(crate::wms::all_wms_providers());
            let custom_cfg = crate::custom_source::load_config();
            providers.extend(crate::custom_source::create_providers(&custom_cfg));
            let provider = providers
                .iter()
                .find(|p| p.info().id == provider_id_clone);

            let result = match provider {
                Some(p) => {
                    if let Some(d) = date {
                        p.fetch(&d, &cache_dir_path)
                    } else {
                        match p.fetch_with_fallback(&cache_dir_path) {
                            Ok((img, _date)) => Ok(img),
                            Err(e) => Err(e),
                        }
                    }
                }
                None => Err(format!("Provider '{}' not found in thread", provider_id_clone)),
            };

            let download_label = if let Some(d) = date {
                format!("{} ({})", label, d.format("%Y-%m-%d"))
            } else {
                label
            };

            let _ = tx.send(DownloadResult {
                provider_id: provider_id_clone,
                label: download_label,
                opacity,
                enabled,
                result,
            });
        });

        self.pending.push(rx);
        log::info!("Download started for provider '{}'", pid);
    }

    /// Polls all pending downloads for completed results.
    pub fn poll(&mut self) -> Vec<DownloadResult> {
        let mut completed = Vec::new();
        let mut still_pending = Vec::new();

        for rx in self.pending.drain(..) {
            match rx.try_recv() {
                Ok(result) => completed.push(result),
                Err(mpsc::TryRecvError::Empty) => still_pending.push(rx),
                Err(mpsc::TryRecvError::Disconnected) => {
                    log::warn!("Download thread disconnected unexpectedly");
                }
            }
        }

        self.pending = still_pending;
        completed
    }
}

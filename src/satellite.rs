// =============================================================================
// Orbis — Satellite Tracking Module (M13)
// =============================================================================
// Downloads Orbit Mean-Elements Messages (OMM) from CelesTrak,
// propagates satellite positions via SGP4/SDP4, and converts
// TEME coordinates to geodetic (lat/lon/alt) for globe rendering.
//
// Coordinate pipeline:
//   SGP4 → TEME (km) → GMST rotation → ECEF (km) → geodetic (°, km)
//
// References:
//   - SGP4/SDP4: Vallado et al., "Revisiting Spacetrack Report #3"
//   - GMST: IAU 1982 model (sufficient for SGP4 accuracy)
//   - WGS84 ellipsoid for ECEF → geodetic conversion
// =============================================================================

use std::sync::mpsc;
use std::time::Instant;

/// WGS84 equatorial radius in km.
const EARTH_RADIUS_KM: f64 = 6378.137;

/// WGS84 flattening factor.
const EARTH_FLATTENING: f64 = 1.0 / 298.257223563;

// =============================================================================
// Satellite Definitions
// =============================================================================

/// A satellite to track, identified by NORAD catalog number.
#[derive(Debug, Clone)]
pub struct SatelliteDef {
    /// Display name (e.g. "ISS")
    pub name: String,
    /// NORAD catalog number (e.g. 25544 for ISS)
    pub norad_id: u32,
    /// Category for grouping in the GUI
    pub category: SatelliteCategory,
}

/// Categories for satellite grouping.
#[derive(Debug, Clone, Copy, PartialEq)]
#[allow(dead_code)] // variants used as data, will be used for GUI grouping in M13d
pub enum SatelliteCategory {
    /// Crewed spacecraft
    Station,
    /// Earth observation satellites
    EarthObservation,
    /// Science / telescopes
    Science,
    /// Communication constellations
    Communication,
}

/// Built-in satellite catalog.
pub fn builtin_satellites() -> Vec<SatelliteDef> {
    vec![
        SatelliteDef { name: "ISS (Zarya)".into(),  norad_id: 25544, category: SatelliteCategory::Station },
        SatelliteDef { name: "CSS (Tianhe)".into(), norad_id: 48274, category: SatelliteCategory::Station },
        SatelliteDef { name: "Hubble (HST)".into(), norad_id: 20580, category: SatelliteCategory::Science },
        SatelliteDef { name: "Landsat 8".into(),    norad_id: 39084, category: SatelliteCategory::EarthObservation },
        SatelliteDef { name: "Landsat 9".into(),    norad_id: 49260, category: SatelliteCategory::EarthObservation },
        SatelliteDef { name: "Sentinel-2A".into(),  norad_id: 40697, category: SatelliteCategory::EarthObservation },
        SatelliteDef { name: "NOAA-20".into(),      norad_id: 43013, category: SatelliteCategory::EarthObservation },
        SatelliteDef { name: "Terra (EOS)".into(),  norad_id: 25994, category: SatelliteCategory::EarthObservation },
    ]
}

// =============================================================================
// Computed Satellite State
// =============================================================================

/// Position and velocity of a satellite at a given time.
#[derive(Debug, Clone)]
pub struct SatelliteState {
    /// Display name
    pub name: String,
    /// NORAD catalog number
    pub norad_id: u32,
    /// Geodetic latitude in degrees (-90..+90)
    pub latitude: f64,
    /// Geodetic longitude in degrees (-180..+180)
    pub longitude: f64,
    /// Altitude above WGS84 ellipsoid in km
    pub altitude_km: f64,
    /// Ground speed in km/s
    pub velocity_km_s: f64,
    /// Category (used for GUI grouping in M13d)
    #[allow(dead_code)]
    pub category: SatelliteCategory,
}

// =============================================================================
// Ground Track (orbit path)
// =============================================================================

/// A point on the ground track (sub-satellite point over time).
#[derive(Debug, Clone, Copy)]
pub struct GroundTrackPoint {
    pub latitude: f64,
    pub longitude: f64,
    /// Minutes relative to current epoch (negative = past)
    pub minutes_offset: f64,
}

// =============================================================================
// Satellite Tracker
// =============================================================================

/// Internal entry: loaded satellite with propagation constants.
struct TrackedSatellite {
    def: SatelliteDef,
    constants: sgp4::Constants,
    elements: sgp4::Elements,
}

/// Background download result.
struct OmmDownloadResult {
    norad_id: u32,
    name: String,
    category: SatelliteCategory,
    elements: sgp4::Elements,
}

/// Manages satellite TLE/OMM data, propagation, and state.
pub struct SatelliteTracker {
    /// Currently tracked satellites with propagation constants.
    tracked: Vec<TrackedSatellite>,
    /// Computed states (updated every frame).
    states: Vec<SatelliteState>,
    /// Receiver for background OMM downloads.
    download_rx: Option<mpsc::Receiver<Result<Vec<OmmDownloadResult>, String>>>,
    /// Last successful OMM refresh time.
    last_refresh: Option<Instant>,
    /// Whether a download is in progress.
    downloading: bool,
}

impl SatelliteTracker {
    pub fn new() -> Self {
        Self {
            tracked: Vec::new(),
            states: Vec::new(),
            download_rx: None,
            last_refresh: None,
            downloading: false,
        }
    }

    /// Starts a background download of OMM data for the built-in satellites.
    pub fn request_refresh(&mut self) {
        if self.downloading {
            return;
        }
        self.downloading = true;

        let satellites = builtin_satellites();
        let (tx, rx) = mpsc::channel();
        self.download_rx = Some(rx);

        std::thread::spawn(move || {
            let result = download_omm_batch(&satellites);
            let _ = tx.send(result);
        });

        log::info!("Satellite OMM download started for {} satellites", builtin_satellites().len());
    }

    /// Polls for completed OMM downloads. Call once per frame.
    pub fn poll_downloads(&mut self) {
        let rx = match &self.download_rx {
            Some(rx) => rx,
            None => return,
        };

        match rx.try_recv() {
            Ok(Ok(results)) => {
                self.tracked.clear();
                for r in results {
                    match sgp4::Constants::from_elements(&r.elements) {
                        Ok(constants) => {
                            self.tracked.push(TrackedSatellite {
                                def: SatelliteDef {
                                    name: r.name.clone(),
                                    norad_id: r.norad_id,
                                    category: r.category,
                                },
                                constants,
                                elements: r.elements,
                            });
                        }
                        Err(e) => {
                            log::warn!("SGP4 init failed for {}: {:?}", r.name, e);
                        }
                    }
                }
                log::info!("Loaded {} satellite OMMs", self.tracked.len());
                self.last_refresh = Some(Instant::now());
                self.downloading = false;
                self.download_rx = None;
            }
            Ok(Err(e)) => {
                log::error!("Satellite OMM download failed: {}", e);
                self.downloading = false;
                self.download_rx = None;
            }
            Err(mpsc::TryRecvError::Empty) => {} // still downloading
            Err(mpsc::TryRecvError::Disconnected) => {
                self.downloading = false;
                self.download_rx = None;
            }
        }
    }

    /// Propagates all tracked satellites to the given UTC time.
    ///
    /// Updates `self.states` with current positions.
    pub fn propagate(&mut self, utc: &chrono::DateTime<chrono::Utc>) {
        self.states.clear();
        let now_jd = utc_to_jd(utc);

        for sat in &self.tracked {
            // Calculate minutes since TLE epoch using chrono
            let epoch_naive = sat.elements.datetime;
            let now_naive = utc.naive_utc();
            let duration = now_naive.signed_duration_since(epoch_naive);
            let minutes = duration.num_milliseconds() as f64 / 60_000.0;

            match sat.constants.propagate(sgp4::MinutesSinceEpoch(minutes)) {
                Ok(prediction) => {
                    // TEME → geodetic
                    let gmst = greenwich_mean_sidereal_time(now_jd);
                    let (lat, lon, alt) = teme_to_geodetic(
                        &prediction.position,
                        gmst,
                    );
                    let vel = (prediction.velocity[0].powi(2)
                        + prediction.velocity[1].powi(2)
                        + prediction.velocity[2].powi(2))
                    .sqrt();

                    self.states.push(SatelliteState {
                        name: sat.def.name.clone(),
                        norad_id: sat.def.norad_id,
                        latitude: lat,
                        longitude: lon,
                        altitude_km: alt,
                        velocity_km_s: vel,
                        category: sat.def.category,
                    });
                }
                Err(e) => {
                    log::trace!("SGP4 propagation error for {}: {:?}", sat.def.name, e);
                }
            }
        }
    }

    /// Computes a ground track for a given satellite.
    ///
    /// Returns sub-satellite points from `past_minutes` before now
    /// to `future_minutes` after now, sampled every `step_minutes`.
    pub fn compute_ground_track(
        &self,
        norad_id: u32,
        utc: &chrono::DateTime<chrono::Utc>,
        past_minutes: f64,
        future_minutes: f64,
        step_minutes: f64,
    ) -> Vec<GroundTrackPoint> {
        let sat = match self.tracked.iter().find(|s| s.def.norad_id == norad_id) {
            Some(s) => s,
            None => return Vec::new(),
        };

        let now_naive = utc.naive_utc();
        let epoch_naive = sat.elements.datetime;
        let base_duration = now_naive.signed_duration_since(epoch_naive);
        let base_minutes = base_duration.num_milliseconds() as f64 / 60_000.0;
        let now_jd = utc_to_jd(utc);

        let mut track = Vec::new();
        let mut t = -past_minutes;
        while t <= future_minutes {
            let total_min = base_minutes + t;
            if let Ok(pred) = sat.constants.propagate(sgp4::MinutesSinceEpoch(total_min)) {
                let offset_jd = now_jd + t / 1440.0;
                let gmst = greenwich_mean_sidereal_time(offset_jd);
                let (lat, lon, _alt) = teme_to_geodetic(&pred.position, gmst);
                track.push(GroundTrackPoint {
                    latitude: lat,
                    longitude: lon,
                    minutes_offset: t,
                });
            }
            t += step_minutes;
        }
        track
    }

    /// Returns current satellite states (read-only).
    pub fn states(&self) -> &[SatelliteState] {
        &self.states
    }

    /// Whether a download is in progress.
    pub fn is_downloading(&self) -> bool {
        self.downloading
    }

    /// Number of tracked satellites.
    pub fn count(&self) -> usize {
        self.tracked.len()
    }
}

// =============================================================================
// OMM Download (CelesTrak)
// =============================================================================

/// Downloads OMM data for multiple satellites from CelesTrak.
///
/// Uses the GP (General Perturbations) API with JSON format.
/// Requests run in parallel threads for fast loading (~2-3s total
/// instead of 30-60s sequential).
fn download_omm_batch(satellites: &[SatelliteDef]) -> Result<Vec<OmmDownloadResult>, String> {
    // Spawn one thread per satellite for parallel downloads
    let handles: Vec<_> = satellites
        .iter()
        .map(|sat| {
            let norad_id = sat.norad_id;
            let name = sat.name.clone();
            let category = sat.category;
            std::thread::spawn(move || -> Option<OmmDownloadResult> {
                let url = format!(
                    "https://celestrak.org/NORAD/elements/gp.php?CATNR={}&FORMAT=json",
                    norad_id
                );
                let response = ureq::get(&url).call().ok()?;
                let body = response.into_body().read_to_string().ok()?;
                let elements_vec: Vec<sgp4::Elements> =
                    serde_json::from_str(&body).ok()?;
                let elem = elements_vec.into_iter().next()?;
                let display_name = elem.object_name.clone().unwrap_or(name.clone());
                log::debug!("OMM OK: {} (NORAD {})", display_name, norad_id);
                Some(OmmDownloadResult {
                    norad_id,
                    name: display_name,
                    category,
                    elements: elem,
                })
            })
        })
        .collect();

    // Collect results
    let mut results = Vec::new();
    for handle in handles {
        if let Ok(Some(result)) = handle.join() {
            results.push(result);
        }
    }

    if results.is_empty() {
        Err("No satellite OMMs could be downloaded".into())
    } else {
        Ok(results)
    }
}

// =============================================================================
// Coordinate Transformations
// =============================================================================

/// Converts a UTC DateTime to Julian Date.
fn utc_to_jd(utc: &chrono::DateTime<chrono::Utc>) -> f64 {
    use chrono::Datelike;
    use chrono::Timelike;

    let y = utc.year() as f64;
    let m = utc.month() as f64;
    let d = utc.day() as f64;
    let h = utc.hour() as f64;
    let min = utc.minute() as f64;
    let s = utc.second() as f64 + utc.nanosecond() as f64 / 1e9;

    // Standard Julian Date formula
    let (y2, m2) = if m <= 2.0 { (y - 1.0, m + 12.0) } else { (y, m) };
    let a = (y2 / 100.0).floor();
    let b = 2.0 - a + (a / 4.0).floor();

    (365.25 * (y2 + 4716.0)).floor()
        + (30.6001 * (m2 + 1.0)).floor()
        + d
        + (h + min / 60.0 + s / 3600.0) / 24.0
        + b
        - 1524.5
}

/// Greenwich Mean Sidereal Time (radians) from Julian Date.
///
/// IAU 1982 model — sufficient accuracy for SGP4.
fn greenwich_mean_sidereal_time(jd: f64) -> f64 {
    let t_ut1 = (jd - 2451545.0) / 36525.0;
    let mut gmst = 67310.54841
        + (876600.0 * 3600.0 + 8640184.812866) * t_ut1
        + 0.093104 * t_ut1 * t_ut1
        - 6.2e-6 * t_ut1 * t_ut1 * t_ut1;
    // Convert seconds → radians, wrap to [0, 2π)
    gmst = (gmst % 86400.0) / 86400.0 * std::f64::consts::TAU;
    if gmst < 0.0 {
        gmst += std::f64::consts::TAU;
    }
    gmst
}

/// Converts TEME position (km) to geodetic coordinates (degrees, km).
///
/// Returns (latitude_deg, longitude_deg, altitude_km).
fn teme_to_geodetic(
    position_km: &[f64; 3],
    gmst_rad: f64,
) -> (f64, f64, f64) {
    let x = position_km[0];
    let y = position_km[1];
    let z = position_km[2];

    // TEME → ECEF: rotate around Z axis by -GMST
    let cos_g = gmst_rad.cos();
    let sin_g = gmst_rad.sin();
    let x_ecef = cos_g * x + sin_g * y;
    let y_ecef = -sin_g * x + cos_g * y;
    let z_ecef = z;

    // ECEF → geodetic (iterative method, WGS84)
    let lon = y_ecef.atan2(x_ecef).to_degrees();
    let p = (x_ecef * x_ecef + y_ecef * y_ecef).sqrt();
    let e2 = EARTH_FLATTENING * (2.0 - EARTH_FLATTENING);

    // Iterative latitude (Bowring's method, converges in 2-3 iterations)
    let mut lat = z_ecef.atan2(p * (1.0 - e2));
    for _ in 0..5 {
        let sin_lat = lat.sin();
        let n = EARTH_RADIUS_KM / (1.0 - e2 * sin_lat * sin_lat).sqrt();
        lat = z_ecef.atan2(p * (1.0 - e2 * n / (n + (p / lat.cos() - n))));
    }

    let sin_lat = lat.sin();
    let n = EARTH_RADIUS_KM / (1.0 - e2 * sin_lat * sin_lat).sqrt();
    let alt = if lat.cos().abs() > 1e-10 {
        p / lat.cos() - n
    } else {
        z_ecef.abs() - n * (1.0 - e2)
    };

    (lat.to_degrees(), lon, alt)
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_utc_to_jd() {
        // J2000 epoch: 2000-01-01 12:00 UTC = JD 2451545.0
        let j2000 = chrono::DateTime::parse_from_rfc3339("2000-01-01T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let jd = utc_to_jd(&j2000);
        assert!((jd - 2451545.0).abs() < 0.001, "J2000 JD should be 2451545.0, got {}", jd);
    }

    #[test]
    fn test_gmst_sanity() {
        // At J2000, GMST ≈ 4.894961 rad (280.46° ≈ 4.894 rad)
        let gmst = greenwich_mean_sidereal_time(2451545.0);
        assert!(gmst > 4.0 && gmst < 5.5, "GMST at J2000 should be ~4.89 rad, got {}", gmst);
    }

    #[test]
    fn test_teme_to_geodetic_equator() {
        // A point on the equator at prime meridian: ECEF = (R, 0, 0)
        // At GMST=0, TEME = ECEF
        let r = EARTH_RADIUS_KM + 400.0; // 400 km altitude
        let (lat, lon, alt) = teme_to_geodetic(&[r, 0.0, 0.0], 0.0);
        assert!(lat.abs() < 1.0, "Equator latitude should be ~0, got {}", lat);
        assert!(lon.abs() < 1.0, "Prime meridian longitude should be ~0, got {}", lon);
        assert!((alt - 400.0).abs() < 10.0, "Altitude should be ~400 km, got {}", alt);
    }

    #[test]
    fn test_teme_to_geodetic_pole() {
        // North pole: ECEF = (0, 0, R_polar)
        let r_polar = EARTH_RADIUS_KM * (1.0 - EARTH_FLATTENING) + 400.0;
        let (lat, _lon, alt) = teme_to_geodetic(&[0.0, 0.0, r_polar], 0.0);
        assert!((lat - 90.0).abs() < 1.0, "North pole lat should be ~90, got {}", lat);
        assert!((alt - 400.0).abs() < 20.0, "Altitude should be ~400 km, got {}", alt);
    }
}

// =============================================================================
// Orbis — Planet Positions (M14b)
// =============================================================================
// Computes geocentric positions for naked-eye planets + Moon using
// simplified Jean Meeus algorithms ("Astronomical Algorithms").
//
// Accuracy: ~1-2° — sufficient for visual placement on the sky sphere.
// The positions are returned as RA/Dec which are then converted to
// 3D coordinates on the sky sphere for rendering.
//
// Planets: Mercury, Venus, Mars, Jupiter, Saturn, Moon
// =============================================================================

/// Sky sphere radius — must match stars.bin and mesh::generate_starfield_fallback.
const SKY_RADIUS: f32 = 50.0;

/// A computed planet position for a given instant.
pub struct PlanetState {
    /// Display name
    pub name: &'static str,
    /// Right ascension in radians (0..2π)
    pub ra: f64,
    /// Declination in radians (-π/2..π/2)
    pub dec: f64,
    /// Apparent visual magnitude (for sizing)
    pub magnitude: f64,
    /// Display color (RGB, 0..1)
    pub color: [f32; 3],
}

impl PlanetState {
    /// Converts RA/Dec to a 3D position on the sky sphere.
    ///
    /// Uses the same coordinate system as the star catalog:
    /// +X = vernal equinox (RA=0), +Y = north celestial pole, +Z = RA=6h
    pub fn to_sky_position(&self) -> [f32; 3] {
        let ra = self.ra as f32;
        let dec = self.dec as f32;
        [
            SKY_RADIUS * dec.cos() * ra.cos(),
            SKY_RADIUS * dec.sin(),
            SKY_RADIUS * dec.cos() * ra.sin(),
        ]
    }

    /// Marker radius in screen pixels based on magnitude.
    pub fn marker_radius(&self) -> f32 {
        // Planets are brighter than most stars → larger markers
        // Venus at -4.5 → 8px, Saturn at +1 → 4px
        let r = 8.0 - self.magnitude as f32;
        r.clamp(3.0, 10.0)
    }
}

/// Computes planet positions for a given UTC date/time.
///
/// Returns positions for Mercury, Venus, Mars, Jupiter, Saturn, and the Moon.
pub fn compute_planet_positions(
    year: i32, month: u32, day: u32,
    hour: u32, minute: u32, second: u32,
) -> Vec<PlanetState> {
    let jd = julian_date(year, month, day, hour, minute, second);
    let t = (jd - 2451545.0) / 36525.0; // Julian centuries since J2000

    // Earth's heliocentric position (needed for geocentric conversion)
    let earth = heliocentric_earth(t);

    let mut planets = Vec::with_capacity(6);

    // --- Inner and outer planets ---
    for &(name, color, compute_fn) in &PLANET_TABLE {
        let (lon_h, lat_h, r_h) = compute_fn(t);
        let (ra, dec, mag) = helio_to_geocentric(lon_h, lat_h, r_h, &earth, name);
        planets.push(PlanetState { name, ra, dec, magnitude: mag, color });
    }

    // --- Moon (special calculation) ---
    let (ra, dec) = lunar_position(t);
    planets.push(PlanetState {
        name: "Moon",
        ra,
        dec,
        magnitude: -12.0, // Full moon approx
        color: [0.95, 0.93, 0.85],
    });

    planets
}

// =============================================================================
// Planet orbital element functions
// =============================================================================

type PlanetFn = fn(f64) -> (f64, f64, f64); // (lon_helio, lat_helio, radius_au)

const PLANET_TABLE: [(&str, [f32; 3], PlanetFn); 5] = [
    ("Mercury", [0.78, 0.75, 0.70], helio_mercury),
    ("Venus",   [0.98, 0.95, 0.80], helio_venus),
    ("Mars",    [1.00, 0.60, 0.40], helio_mars),
    ("Jupiter", [0.90, 0.85, 0.70], helio_jupiter),
    ("Saturn",  [0.90, 0.82, 0.60], helio_saturn),
];

/// Simplified heliocentric ecliptic coordinates for Earth.
fn heliocentric_earth(t: f64) -> (f64, f64, f64) {
    let l = norm_deg(100.46646 + 36000.76983 * t);
    let m = norm_deg(357.52911 + 35999.05029 * t).to_radians();
    let c = (1.9146 - 0.004817 * t) * m.sin()
        + 0.019993 * (2.0 * m).sin();
    let lon = (l + c).to_radians();
    let r = 1.000_140 - 0.016_708 * m.cos() - 0.000_140 * (2.0 * m).cos();
    (lon, 0.0, r)
}

fn helio_mercury(t: f64) -> (f64, f64, f64) {
    let l = norm_deg(252.2509 + 149472.6746 * t);
    let m = norm_deg(174.7948 + 149472.5153 * t).to_radians();
    let c = 23.4400 * m.sin() + 2.9818 * (2.0 * m).sin() + 0.5255 * (3.0 * m).sin();
    let lon = (l + c).to_radians();
    let r = 0.395_10 - 0.077_10 * m.cos() - 0.009_05 * (2.0 * m).cos();
    (lon, 0.0, r) // lat ≈ 0 for simplified
}

fn helio_venus(t: f64) -> (f64, f64, f64) {
    let l = norm_deg(181.9798 + 58517.8157 * t);
    let m = norm_deg(50.4161 + 58517.8039 * t).to_radians();
    let c = 0.7758 * m.sin() + 0.0033 * (2.0 * m).sin();
    let lon = (l + c).to_radians();
    let r = 0.723_33 - 0.004_96 * m.cos();
    (lon, 0.0, r)
}

fn helio_mars(t: f64) -> (f64, f64, f64) {
    let l = norm_deg(355.4330 + 19140.2993 * t);
    let m = norm_deg(19.3730 + 19139.8585 * t).to_radians();
    let c = 10.6912 * m.sin() + 0.6228 * (2.0 * m).sin() + 0.0503 * (3.0 * m).sin();
    let lon = (l + c).to_radians();
    let r = 1.530_33 - 0.141_70 * m.cos() - 0.006_93 * (2.0 * m).cos();
    (lon, 0.0, r)
}

fn helio_jupiter(t: f64) -> (f64, f64, f64) {
    let l = norm_deg(34.3515 + 3034.9057 * t);
    let m = norm_deg(19.8950 + 3034.6845 * t).to_radians();
    let c = 5.5549 * m.sin() + 0.1683 * (2.0 * m).sin();
    let lon = (l + c).to_radians();
    let r = 5.202_79 - 0.252_78 * m.cos() - 0.001_40 * (2.0 * m).cos();
    (lon, 0.0, r)
}

fn helio_saturn(t: f64) -> (f64, f64, f64) {
    let l = norm_deg(50.0774 + 1222.1138 * t);
    let m = norm_deg(317.0207 + 1222.1138 * t).to_radians();
    let c = 6.3585 * m.sin() + 0.2204 * (2.0 * m).sin();
    let lon = (l + c).to_radians();
    let r = 9.554_75 - 0.536_40 * m.cos() - 0.012_35 * (2.0 * m).cos();
    (lon, 0.0, r)
}

// =============================================================================
// Geocentric conversion
// =============================================================================

/// Converts heliocentric ecliptic → geocentric equatorial (RA, Dec).
///
/// Also computes a rough apparent magnitude.
fn helio_to_geocentric(
    lon_p: f64, _lat_p: f64, r_p: f64,
    earth: &(f64, f64, f64),
    name: &str,
) -> (f64, f64, f64) {
    let (lon_e, _lat_e, r_e) = *earth;

    // Geocentric ecliptic cartesian
    let x = r_p * lon_p.cos() - r_e * lon_e.cos();
    let y = r_p * lon_p.sin() - r_e * lon_e.sin();
    let z = 0.0; // simplified: no ecliptic latitude

    // Ecliptic → equatorial (rotate by obliquity ε ≈ 23.44°)
    let eps = 23.4393_f64.to_radians();
    let x_eq = x;
    let y_eq = y * eps.cos() - z * eps.sin();
    let z_eq = y * eps.sin() + z * eps.cos();

    let ra = y_eq.atan2(x_eq);
    let ra = if ra < 0.0 { ra + std::f64::consts::TAU } else { ra };
    let dec = z_eq.atan2((x_eq * x_eq + y_eq * y_eq).sqrt());

    // Rough distance for magnitude estimate
    let dist = (x * x + y * y + z * z).sqrt();

    // Approximate magnitude (very rough)
    let base_mag = match name {
        "Mercury" => -0.4,
        "Venus"   => -4.4,
        "Mars"    => -1.5,
        "Jupiter" => -2.5,
        "Saturn"  => 0.5,
        _         => 0.0,
    };
    let mag = base_mag + 5.0 * dist.log10();

    (ra, dec, mag)
}

// =============================================================================
// Moon position (simplified)
// =============================================================================

/// Simplified lunar position (geocentric RA/Dec).
///
/// Based on Meeus Chapter 47, reduced to main terms.
fn lunar_position(t: f64) -> (f64, f64) {
    // Mean elements (degrees)
    let l_prime = norm_deg(218.3165 + 481267.8813 * t); // Mean longitude
    let d = norm_deg(297.8502 + 445267.1115 * t);       // Mean elongation
    let m = norm_deg(357.5291 + 35999.0503 * t);        // Sun mean anomaly
    let m_prime = norm_deg(134.9634 + 477198.8676 * t);  // Moon mean anomaly
    let f = norm_deg(93.2721 + 483202.0175 * t);         // Argument of latitude

    let d = d.to_radians();
    let m = m.to_radians();
    let mp = m_prime.to_radians();
    let f = f.to_radians();

    // Ecliptic longitude (main terms only)
    let lambda = l_prime
        + 6.289 * mp.sin()
        - 1.274 * (2.0 * d - mp).sin()
        + 0.658 * (2.0 * d).sin()
        - 0.214 * (2.0 * mp).sin()
        - 0.186 * m.sin()
        - 0.114 * (2.0 * f).sin();

    // Ecliptic latitude (main terms only)
    let beta = 5.128 * f.sin()
        + 0.281 * (mp + f).sin()
        - 0.278 * (mp - f).sin()
        - 0.173 * (2.0 * d - f).sin();

    let lambda = lambda.to_radians();
    let beta = beta.to_radians();

    // Ecliptic → equatorial
    let eps = 23.4393_f64.to_radians();

    let ra = (lambda.sin() * eps.cos() - beta.tan() * eps.sin())
        .atan2(lambda.cos());
    let ra = if ra < 0.0 { ra + std::f64::consts::TAU } else { ra };

    let dec = (beta.cos() * eps.sin() * lambda.sin() + beta.sin() * eps.cos()).asin();

    (ra, dec)
}

// =============================================================================
// Utility
// =============================================================================

/// Normalizes an angle to 0..360°.
fn norm_deg(deg: f64) -> f64 {
    let mut d = deg % 360.0;
    if d < 0.0 { d += 360.0; }
    d
}

/// Re-export from sun.rs to avoid duplication.
fn julian_date(year: i32, month: u32, day: u32, hour: u32, min: u32, sec: u32) -> f64 {
    crate::sun::julian_date(year, month, day, hour, min, sec)
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_planets_return_six() {
        let planets = compute_planet_positions(2026, 3, 17, 12, 0, 0);
        assert_eq!(planets.len(), 6, "Should return 5 planets + Moon");
    }

    #[test]
    fn test_ra_dec_in_range() {
        let planets = compute_planet_positions(2026, 3, 17, 12, 0, 0);
        for p in &planets {
            assert!(p.ra >= 0.0 && p.ra < std::f64::consts::TAU,
                "{} RA out of range: {}", p.name, p.ra);
            assert!(p.dec >= -std::f64::consts::FRAC_PI_2 && p.dec <= std::f64::consts::FRAC_PI_2,
                "{} Dec out of range: {}", p.name, p.dec);
        }
    }

    #[test]
    fn test_moon_moves_between_dates() {
        let p1 = compute_planet_positions(2026, 3, 17, 0, 0, 0);
        let p2 = compute_planet_positions(2026, 3, 20, 0, 0, 0);
        let moon1 = p1.iter().find(|p| p.name == "Moon").unwrap();
        let moon2 = p2.iter().find(|p| p.name == "Moon").unwrap();
        // Moon moves ~13°/day → 3 days ≈ 39° ≈ 0.68 rad
        let delta_ra = (moon2.ra - moon1.ra).abs();
        assert!(delta_ra > 0.3, "Moon should move significantly in 3 days, delta_ra={}", delta_ra);
    }
}

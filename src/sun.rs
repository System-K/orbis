// =============================================================================
// Orbis — Sun Position (M3a)
// =============================================================================
// Computes the direction to the sun as a 3D vector from the current UTC time.
//
// Astronomy summary:
// 1. UTC → Julian Date (continuous day counter since 4713 BC)
// 2. Julian Date → Ecliptic longitude of the sun (where is the sun
//    on its apparent path across the sky?)
// 3. Ecliptic longitude → Right ascension + declination (sky coordinates,
//    similar to longitude/latitude, but on the celestial sphere)
// 4. Right ascension + hour angle → sub-solar point on Earth's surface
//    (the point where the sun is directly overhead)
// 5. Sub-solar point → 3D direction vector for the shader
//
// The calculation uses a simplified solar position after Jean Meeus
// ("Astronomical Algorithms"). Accuracy: ~1° — more than enough for
// a visual day/night representation.
//
// Important: In our coordinate system (matching mesh.rs):
//   +Y = North Pole, -Y = South Pole
//   +X = 180°W (Date Line, texture left edge, u=0)
//   -X = 0° (Greenwich/Prime Meridian, texture center, u=0.5)
//   +Z = 90°W (u=0.25), -Z = 90°E (u=0.75)
// =============================================================================

use chrono::{Datelike, Timelike, Utc};
use glam::Vec3;

/// Computes the current sun direction as a normalized 3D vector.
///
/// Returns the direction FROM the Earth TO the sun.
/// The shader uses this as light direction: points whose normal
/// faces the sun are lit (day), the rest is dark (night).
pub fn sun_direction_now() -> Vec3 {
    let now = Utc::now();
    sun_direction_at(
        now.year(),
        now.month(),
        now.day(),
        now.hour(),
        now.minute(),
        now.second(),
    )
}

/// Computes the sun direction for any date/time (UTC).
///
/// Public so we can implement a time-travel feature later
/// (slider in the GUI → choose any date).
pub fn sun_direction_at(
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
) -> Vec3 {
    // =========================================================================
    // Step 1: Julian Date (JD)
    // =========================================================================
    // The JD is a continuous day counter universally used in astronomy.
    // No time zones, no leap year problems.
    // JD 2451545.0 = January 1, 2000, 12:00 UTC (the "J2000.0" epoch)
    let jd = julian_date(year, month, day, hour, minute, second);

    // Centuries since J2000.0 — most formulas use this measure.
    let t = (jd - 2451545.0) / 36525.0;

    // =========================================================================
    // Step 2: Mean anomaly + ecliptic longitude of the sun
    // =========================================================================
    // The "mean anomaly" M describes how far Earth is along its
    // (approximated circular) orbit around the sun.
    // The "ecliptic longitude" λ corrects for the elliptical orbit shape.

    // Mean longitude of the sun (degrees)
    let l0 = (280.46646 + 36000.76983 * t) % 360.0;

    // Mean anomaly (degrees)
    let m_deg = (357.52911 + 35999.05029 * t) % 360.0;
    let m = m_deg.to_radians();

    // Equation of center (correction circle → ellipse)
    let c = (1.9146 - 0.004817 * t) * m.sin()
        + 0.019993 * (2.0 * m).sin()
        + 0.00029 * (3.0 * m).sin();

    // Ecliptic longitude of the sun (degrees)
    let sun_lon_deg = (l0 + c) % 360.0;
    let sun_lon = sun_lon_deg.to_radians();

    // =========================================================================
    // Step 3: Right Ascension (RA) and Declination (Dec)
    // =========================================================================
    // Earth's axis is tilted ~23.44° against the orbital plane.
    // This "obliquity of the ecliptic" causes the seasons
    // and affects how high the sun appears in the sky.

    // Obliquity of the ecliptic (slow change over millennia)
    let obliquity = (23.439 - 0.00000036 * (jd - 2451545.0)).to_radians();

    // Declination: How far north/south the sun is in the sky.
    // June 21: Dec ≈ +23.44° (sun over Tropic of Cancer)
    // Dec 21: Dec ≈ -23.44° (sun over Tropic of Capricorn)
    let sin_dec = obliquity.sin() * sun_lon.sin();
    let dec = sin_dec.asin();

    // Right ascension: Where on the celestial equator the sun is (0..2π).
    // Together with the time of day, determines over which longitude
    // the sun is directly overhead.
    let ra = obliquity.cos() * sun_lon.sin();
    let ra = ra.atan2(sun_lon.cos());

    // =========================================================================
    // Step 4: Sub-solar point (longitude + latitude)
    // =========================================================================
    // The sub-solar point is the location on Earth's surface where the
    // sun is exactly at the zenith (directly overhead).
    //
    // Latitude = declination (directly!)
    // Longitude = right ascension - Greenwich sidereal time
    //
    // Sidereal time (GMST) tells us which celestial meridian is currently
    // over Greenwich. Together with RA this gives us the longitude.

    // Greenwich Mean Sidereal Time (GMST) in hours → radians
    let gmst_hours = 18.697374558 + 24.06570982441908 * (jd - 2451545.0);
    let gmst_rad = (gmst_hours % 24.0) / 24.0 * std::f64::consts::TAU;

    // Sub-solar longitude: RA - GMST
    // Normalize to -π..π
    let mut sub_lon = ra - gmst_rad;
    while sub_lon > std::f64::consts::PI {
        sub_lon -= std::f64::consts::TAU;
    }
    while sub_lon < -std::f64::consts::PI {
        sub_lon += std::f64::consts::TAU;
    }

    // Sub-solar latitude = declination
    let sub_lat = dec;

    // =========================================================================
    // Step 5: Geographic coordinates → 3D vector
    // =========================================================================
    // IMPORTANT: In our sphere mesh (mesh.rs):
    //   phi=0   (u=0.00) → x=+1, z= 0 → +X = 180°W (Date Line)
    //   phi=π/2 (u=0.25) → x= 0, z=+1 → +Z =  90°W
    //   phi=π   (u=0.50) → x=-1, z= 0 → -X =   0°  (Greenwich)
    //   phi=3π/2(u=0.75) → x= 0, z=-1 → -Z =  90°E
    //
    // Conversion geo → mesh: phi = lon + π
    //   x = cos(lat) * cos(lon + π) = -cos(lat) * cos(lon)
    //   y = sin(lat)
    //   z = cos(lat) * sin(lon + π) = -cos(lat) * sin(lon)
    let sub_lat = sub_lat as f32;
    let sub_lon = sub_lon as f32;

    Vec3::new(
        -sub_lat.cos() * sub_lon.cos(),
        sub_lat.sin(),
        -sub_lat.cos() * sub_lon.sin(),
    )
    .normalize()
}

/// Converts a calendar date (UTC) to a Julian Date.
///
/// Formula after Meeus, "Astronomical Algorithms", Chapter 7.
/// Works for all dates in the Gregorian calendar (from 1582 onward).
pub fn julian_date(year: i32, month: u32, day: u32, hour: u32, min: u32, sec: u32) -> f64 {
    let (y, m) = if month <= 2 {
        (year as f64 - 1.0, month as f64 + 12.0)
    } else {
        (year as f64, month as f64)
    };

    let a = (y / 100.0).floor();
    let b = 2.0 - a + (a / 4.0).floor();

    let jd = (365.25 * (y + 4716.0)).floor()
        + (30.6001 * (m + 1.0)).floor()
        + day as f64
        + b
        - 1524.5;

    // Day fraction from time
    let day_fraction = (hour as f64 + min as f64 / 60.0 + sec as f64 / 3600.0) / 24.0;

    jd + day_fraction
}

// =============================================================================
// Tests
// =============================================================================
#[cfg(test)]
mod tests {
    use super::*;

    /// Plausibility test: On June 21 (summer solstice) the solar
    /// declination should be near +23.44° → Y component positive and large.
    #[test]
    fn summer_solstice_sun_is_north() {
        let dir = sun_direction_at(2024, 6, 21, 12, 0, 0);
        // Y > 0.35 corresponds to Dec > ~20° (sin(20°) ≈ 0.34)
        assert!(
            dir.y > 0.35,
            "Sun should be north in summer, y={:.3}",
            dir.y
        );
    }

    /// On December 21 (winter solstice): sun is south → Y negative.
    #[test]
    fn winter_solstice_sun_is_south() {
        let dir = sun_direction_at(2024, 12, 21, 12, 0, 0);
        assert!(
            dir.y < -0.35,
            "Sun should be south in winter, y={:.3}",
            dir.y
        );
    }

    /// On March 20 (vernal equinox): sun near equator.
    #[test]
    fn equinox_sun_near_equator() {
        let dir = sun_direction_at(2024, 3, 20, 12, 0, 0);
        assert!(
            dir.y.abs() < 0.05,
            "Sun should be near equator at equinox, y={:.3}",
            dir.y
        );
    }

    /// Sun vector must be normalized (length ≈ 1.0).
    #[test]
    fn sun_vector_is_normalized() {
        let dir = sun_direction_at(2024, 8, 15, 18, 30, 0);
        let len = dir.length();
        assert!(
            (len - 1.0).abs() < 0.001,
            "Vector should be normalized, length={:.6}",
            len
        );
    }
}

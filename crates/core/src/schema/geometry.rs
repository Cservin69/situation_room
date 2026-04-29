//! Geometry types. GeoJSON-compatible but owned here so we don't depend
//! on a heavy GIS crate in Phase 2. The types are designed so future
//! migration to `geo-types` (if we need real spatial operations) can
//! happen with a `From` impl.
//!
//! Coordinates are longitude-then-latitude (GeoJSON order, WGS84), NOT
//! lat-then-lon. This catches people out repeatedly — the type names
//! make it explicit, use [`Position::new(lon, lat)`] and fields
//! [`Position::lon`]/[`Position::lat`] rather than tuple access.

use serde::{Deserialize, Serialize};

/// A geographic position in WGS84, longitude first (GeoJSON convention).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct Position {
    pub lon: f64,
    pub lat: f64,
}

impl Position {
    pub fn new(lon: f64, lat: f64) -> Self {
        Self { lon, lat }
    }

    pub fn is_valid(&self) -> bool {
        self.lon.is_finite()
            && self.lat.is_finite()
            && (-180.0..=180.0).contains(&self.lon)
            && (-90.0..=90.0).contains(&self.lat)
    }
}

/// Geometry — attached as an optional field to Entity, Event, Observation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "PascalCase")]
pub enum Geometry {
    Point(PointGeom),
    LineString(LineStringGeom),
    Polygon(PolygonGeom),
    MultiPolygon(MultiPolygonGeom),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PointGeom {
    pub coordinates: Position,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LineStringGeom {
    pub coordinates: Vec<Position>,
}

/// A polygon: one outer ring followed by zero or more holes. Each ring
/// is a closed ring of positions (last == first). We don't enforce
/// closure here — consumers that need topology (point-in-polygon) can
/// close rings themselves.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PolygonGeom {
    /// `rings[0]` is the outer ring. `rings[1..]` are holes.
    pub coordinates: Vec<Vec<Position>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MultiPolygonGeom {
    pub coordinates: Vec<Vec<Vec<Position>>>,
}

impl Geometry {
    /// Construct a point geometry from longitude, latitude.
    pub fn point(lon: f64, lat: f64) -> Self {
        Self::Point(PointGeom {
            coordinates: Position::new(lon, lat),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_validates() {
        assert!(Position::new(-70.6, -33.4).is_valid()); // Santiago
        assert!(!Position::new(200.0, 0.0).is_valid()); // out of range
        assert!(!Position::new(f64::NAN, 0.0).is_valid());
    }

    #[test]
    fn geometry_roundtrips_as_geojson_shape() {
        let g = Geometry::point(-70.6, -33.4);
        let json = serde_json::to_string(&g).unwrap();
        // Tagged enum with "type" as the discriminator
        assert!(json.contains("\"type\":\"Point\""));
        assert!(json.contains("-70.6"));
        let back: Geometry = serde_json::from_str(&json).unwrap();
        assert_eq!(g, back);
    }
}

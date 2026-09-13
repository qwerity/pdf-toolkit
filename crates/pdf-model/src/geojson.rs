//! The model as map-native extruded GeoJSON: one polygon feature per
//! element with `height` / `base` / `color` properties, which a Mapbox or
//! MapLibre `fill-extrusion` layer draws directly — the same shape the
//! `building` command emits, so a map that draws one draws the other.
//!
//! Coordinates are the model's own metres (x east, y north of the grid
//! origin) unless a [`Placement`] is given, in which case they are
//! longitude/latitude. Rooms are carried flat (no `height`) with their area,
//! for labelling.

use geojson::{Feature, FeatureCollection, Geometry, GeometryValue, JsonObject, Position};
use pdf_geo::local::Placement;
use pdf_geometry::model::Point;
use serde_json::{Value, json};

use crate::elements::{Element, elements};
use crate::geom::round;
use crate::model::{Model, Polygon2D, Ring};

/// The frame the collection is written in.
pub struct Frame<'a> {
    /// Local metres to the ground, when the model was placed.
    pub placement: Option<&'a Placement>,
}

impl Frame<'_> {
    pub fn crs(&self) -> &'static str {
        if self.placement.is_some() {
            "EPSG:4326"
        } else {
            "local_metres"
        }
    }

    fn position(&self, p: [f64; 2]) -> Position {
        match self.placement {
            Some(placement) => {
                let lonlat = placement.to_wgs84(Point::new(p[0], p[1]));
                Position::from([round(lonlat.lon, 7), round(lonlat.lat, 7)])
            }
            None => Position::from([p[0], p[1]]),
        }
    }

    fn ring(&self, ring: &Ring) -> Vec<Position> {
        let mut out: Vec<Position> = ring.iter().map(|&p| self.position(p)).collect();
        if let Some(first) = out.first().cloned() {
            out.push(first);
        }
        out
    }

    fn polygon(&self, polygon: &Polygon2D) -> Geometry {
        let mut rings = vec![self.ring(&polygon.outer)];
        rings.extend(polygon.holes.iter().map(|h| self.ring(h)));
        Geometry::new(GeometryValue::Polygon { coordinates: rings })
    }
}

fn feature(frame: &Frame<'_>, element: &Element<'_>) -> Feature {
    let mut properties = JsonObject::new();
    properties.insert("role".into(), json!(element.role.name()));
    if let Some((index, name)) = element.floor {
        properties.insert("floor".into(), json!(index));
        properties.insert("floor_name".into(), json!(name));
    }
    properties.insert("base".into(), json!(round(element.z[0], 3)));
    properties.insert("height".into(), json!(round(element.z[1], 3)));
    properties.insert("color".into(), json!(element.role.hex()));
    properties.insert("area_m2".into(), json!(round(element.polygon.area_m2(), 3)));
    Feature {
        bbox: None,
        geometry: Some(frame.polygon(&element.polygon)),
        id: None,
        properties: Some(properties),
        foreign_members: None,
    }
}

/// Everything the model draws, as a FeatureCollection. `plinth_m` and
/// `rail_height_m` are parameters rather than model facts, as in the glTF.
pub fn collection(
    model: &Model,
    frame: &Frame<'_>,
    plinth_m: f64,
    rail_height_m: f64,
    metadata: Value,
) -> FeatureCollection {
    let mut features: Vec<Feature> = elements(model, plinth_m, rail_height_m)
        .iter()
        .map(|element| feature(frame, element))
        .collect();

    // Rooms: flat, labelled by area, for a map that wants to name them.
    for (index, floor) in model.floors.iter().enumerate() {
        for room in &floor.rooms {
            let mut properties = JsonObject::new();
            properties.insert("role".into(), json!("room"));
            properties.insert("floor".into(), json!(index));
            properties.insert("floor_name".into(), json!(floor.name));
            properties.insert("level".into(), json!(floor.z));
            properties.insert("area_m2".into(), json!(room.area));
            properties.insert("label".into(), json!(format!("{:.1} m²", room.area)));
            features.push(Feature {
                bbox: None,
                geometry: Some(frame.polygon(&room.polygon)),
                id: None,
                properties: Some(properties),
                foreign_members: None,
            });
        }
    }

    let mut foreign = JsonObject::new();
    let mut metadata = metadata;
    if let Value::Object(map) = &mut metadata {
        map.insert("crs".into(), json!(frame.crs()));
        map.insert("placed".into(), json!(frame.placement.is_some()));
    }
    foreign.insert("metadata".into(), metadata);
    FeatureCollection {
        bbox: None,
        features,
        foreign_members: Some(foreign),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Floor, Room};
    use indexmap::IndexMap;

    fn square(x0: f64, y0: f64, side: f64) -> Polygon2D {
        Polygon2D {
            outer: vec![
                [x0, y0],
                [x0 + side, y0],
                [x0 + side, y0 + side],
                [x0, y0 + side],
            ],
            holes: Vec::new(),
        }
    }

    fn model() -> Model {
        Model {
            units: "m".into(),
            up: "z".into(),
            source: "t".into(),
            scale: "1:75".into(),
            levels: IndexMap::from([("ground".to_string(), 0.0), ("parapet".to_string(), 7.78)]),
            floors: vec![Floor {
                name: "Ground".into(),
                page: 7,
                z: 0.0,
                top: 3.6,
                envelope: Polygon2D::default(),
                slab: square(0.0, 0.0, 10.0),
                slab_z: [-0.35, 0.0],
                walls: vec![square(0.0, 0.0, 0.4)],
                glazing: vec![square(5.0, 0.0, 1.0)],
                glazing_z: [0.0, 3.1],
                door_gaps: Vec::new(),
                rooms: vec![Room {
                    area: 4.0,
                    polygon: square(1.0, 1.0, 2.0),
                }],
                rails: vec![vec![[10.0, 0.0], [10.0, 5.0]]],
            }],
            roof: None,
            stone: vec![square(12.0, 0.0, 0.5)],
        }
    }

    #[test]
    fn local_features_carry_extrusion_properties_and_the_frame() {
        let fc = collection(
            &model(),
            &Frame { placement: None },
            0.35,
            1.1,
            json!({"document": "t"}),
        );
        let roles: Vec<&str> = fc
            .features
            .iter()
            .map(|f| f.properties.as_ref().unwrap()["role"].as_str().unwrap())
            .collect();
        assert_eq!(roles, ["slab", "wall", "glazing", "rail", "stone", "room"]);
        let wall = &fc.features[1].properties.as_ref().unwrap();
        assert_eq!(wall["base"], 0.0);
        assert_eq!(wall["height"], 3.6);
        assert_eq!(wall["color"], "#ebe8e1");
        let stone = &fc.features[4].properties.as_ref().unwrap();
        assert_eq!(stone["base"], -0.35);
        assert_eq!(stone["height"], 7.78);
        let room = &fc.features[5].properties.as_ref().unwrap();
        assert!(room.get("height").is_none(), "rooms are flat");
        assert_eq!(room["label"], "4.0 m²");
        let meta = &fc.foreign_members.as_ref().unwrap()["metadata"];
        assert_eq!(meta["crs"], "local_metres");
        assert_eq!(meta["placed"], false);
        assert_eq!(meta["document"], "t");
        // Rings close.
        let GeometryValue::Polygon { coordinates } =
            &fc.features[0].geometry.as_ref().unwrap().value
        else {
            panic!("polygon")
        };
        assert_eq!(coordinates[0].first(), coordinates[0].last());
        assert_eq!(coordinates[0].len(), 5);
    }

    #[test]
    fn a_placement_turns_metres_into_degrees() {
        use pdf_geo::local::{Reference, place};
        // The model's x axis points east: (0,0) and (10,0) are two references
        // 10 m apart along a parallel at Yerevan.
        let placement = place(&[
            Reference {
                local: [0.0, 0.0],
                wgs84: [44.5, 40.09],
            },
            Reference {
                local: [10.0, 0.0],
                wgs84: [44.5001173, 40.09],
            },
        ])
        .expect("two references place");
        let fc = collection(
            &model(),
            &Frame {
                placement: Some(&placement),
            },
            0.35,
            1.1,
            json!({}),
        );
        let meta = &fc.foreign_members.as_ref().unwrap()["metadata"];
        assert_eq!(meta["crs"], "EPSG:4326");
        let GeometryValue::Polygon { coordinates } =
            &fc.features[0].geometry.as_ref().unwrap().value
        else {
            panic!("polygon")
        };
        let first = &coordinates[0][0];
        assert!((first[0] - 44.5).abs() < 1e-6 && (first[1] - 40.09).abs() < 1e-6);
        let east = &coordinates[0][1];
        assert!(east[0] > 44.5001 && east[0] < 44.50013, "{east:?}");
    }
}

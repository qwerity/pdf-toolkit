//! The model as a triangle mesh: every polygon extruded between its z range
//! and written as a binary glTF (`.glb`) with per-vertex colours, one node
//! per layer, metres, Z up — what Blender, three.js or any glTF viewer
//! loads without a plugin.
//!
//! The writer is deliberately small: positions, vertex colours and indices
//! in one buffer, one primitive per layer. Caps are triangulated with
//! `earcutr` (holes included); each ring becomes a quad strip of side
//! walls.

use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use geo::{Coord, LineString, Polygon};
use indexmap::IndexMap;
use serde_json::{Value, json};

use crate::elements::elements;
use crate::geom;
use crate::model::Model;

/// RGBA in 0..=255.
type Colour = [u8; 4];

/// One layer's triangles, accumulated before packing.
#[derive(Default)]
struct Layer {
    name: String,
    positions: Vec<[f32; 3]>,
    colours: Vec<[f32; 4]>,
    indices: Vec<u32>,
}

impl Layer {
    fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Default::default()
        }
    }

    fn push_vertex(&mut self, x: f64, y: f64, z: f64, colour: Colour) -> u32 {
        self.positions.push([x as f32, y as f32, z as f32]);
        self.colours.push(colour.map(|c| f32::from(c) / 255.0));
        (self.positions.len() - 1) as u32
    }

    /// Extrudes a polygon (holes included) from `z0` to `z1`.
    fn extrude(&mut self, polygon: &Polygon<f64>, z0: f64, z1: f64, colour: Colour) {
        let rings: Vec<&LineString<f64>> = std::iter::once(polygon.exterior())
            .chain(polygon.interiors())
            .collect();
        let open_rings: Vec<Vec<Coord<f64>>> = rings
            .iter()
            .map(|ring| {
                let mut coords = ring.0.clone();
                if coords.len() > 1 && coords.first() == coords.last() {
                    coords.pop();
                }
                coords
            })
            .filter(|coords| coords.len() >= 3)
            .collect();
        if open_rings.is_empty() {
            return;
        }

        // Caps: earcut over the flattened outer ring and hole rings.
        let mut flat: Vec<f64> = Vec::new();
        let mut hole_starts: Vec<usize> = Vec::new();
        for (i, coords) in open_rings.iter().enumerate() {
            if i > 0 {
                hole_starts.push(flat.len() / 2);
            }
            for c in coords {
                flat.push(c.x);
                flat.push(c.y);
            }
        }
        let Ok(triangles) = earcutr::earcut(&flat, &hole_starts, 2) else {
            return;
        };
        let all: Vec<Coord<f64>> = open_rings.iter().flatten().copied().collect();
        for (z, flip) in [(z0, true), (z1, false)] {
            let base: Vec<u32> = all
                .iter()
                .map(|c| self.push_vertex(c.x, c.y, z, colour))
                .collect();
            for tri in triangles.chunks(3) {
                let [a, b, c] = [base[tri[0]], base[tri[1]], base[tri[2]]];
                if flip {
                    self.indices.extend([a, c, b]);
                } else {
                    self.indices.extend([a, b, c]);
                }
            }
        }

        // Sides: one quad per ring edge. Outward-facing for a CCW outer ring;
        // a hole ring runs the other way, so its quads face into the hole.
        for coords in &open_rings {
            let n = coords.len();
            for i in 0..n {
                let (p, q) = (coords[i], coords[(i + 1) % n]);
                let a = self.push_vertex(p.x, p.y, z0, colour);
                let b = self.push_vertex(q.x, q.y, z0, colour);
                let c = self.push_vertex(q.x, q.y, z1, colour);
                let d = self.push_vertex(p.x, p.y, z1, colour);
                self.indices.extend([a, b, c, a, c, d]);
            }
        }
    }

    fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }
}

/// The model's layers as meshes: one node per element layer, in the order
/// the extrusion table lists them.
fn layers(model: &Model, plinth_m: f64, rail_height_m: f64) -> Vec<Layer> {
    let mut layers: IndexMap<String, Layer> = IndexMap::new();
    for element in elements(model, plinth_m, rail_height_m) {
        let name = element.layer();
        layers
            .entry(name.clone())
            .or_insert_with(|| Layer::new(name))
            .extrude(
                &geom::from_polygon2d(&element.polygon),
                element.z[0],
                element.z[1],
                element.role.colour(),
            );
    }
    layers.into_values().filter(|l| !l.is_empty()).collect()
}

/// Packs the layers into one GLB: a JSON chunk describing accessors over a
/// single binary buffer of positions, colours and indices.
fn pack_glb(layers: &[Layer]) -> Vec<u8> {
    let mut bin: Vec<u8> = Vec::new();
    let mut buffer_views = Vec::new();
    let mut accessors = Vec::new();
    let mut meshes = Vec::new();
    let mut nodes = Vec::new();

    let mut push_view = |bin: &mut Vec<u8>, bytes: &[u8], target: u32| -> usize {
        while !bin.len().is_multiple_of(4) {
            bin.push(0);
        }
        let offset = bin.len();
        bin.extend_from_slice(bytes);
        buffer_views.push(json!({
            "buffer": 0, "byteOffset": offset, "byteLength": bytes.len(), "target": target
        }));
        buffer_views.len() - 1
    };

    for (index, layer) in layers.iter().enumerate() {
        let positions: Vec<u8> = layer
            .positions
            .iter()
            .flat_map(|p| p.iter().flat_map(|v| v.to_le_bytes()))
            .collect();
        let colours: Vec<u8> = layer
            .colours
            .iter()
            .flat_map(|c| c.iter().flat_map(|v| v.to_le_bytes()))
            .collect();
        let indices: Vec<u8> = layer.indices.iter().flat_map(|i| i.to_le_bytes()).collect();
        let (min, max) = bounds(&layer.positions);

        let position_view = push_view(&mut bin, &positions, 34962);
        let colour_view = push_view(&mut bin, &colours, 34962);
        let index_view = push_view(&mut bin, &indices, 34963);
        accessors.push(json!({
            "bufferView": position_view, "componentType": 5126,
            "count": layer.positions.len(), "type": "VEC3", "min": min, "max": max
        }));
        accessors.push(json!({
            "bufferView": colour_view, "componentType": 5126,
            "count": layer.colours.len(), "type": "VEC4"
        }));
        accessors.push(json!({
            "bufferView": index_view, "componentType": 5125,
            "count": layer.indices.len(), "type": "SCALAR"
        }));
        let base = index * 3;
        meshes.push(json!({
            "name": layer.name,
            "primitives": [{
                "attributes": {"POSITION": base, "COLOR_0": base + 1},
                "indices": base + 2, "mode": 4
            }]
        }));
        nodes.push(json!({"name": layer.name, "mesh": index}));
    }

    let scene_nodes: Vec<usize> = (0..layers.len()).collect();
    let document: Value = json!({
        "asset": {"version": "2.0", "generator": "pdf-toolkit pdf-model"},
        "scene": 0,
        "scenes": [{"nodes": scene_nodes}],
        "nodes": nodes,
        "meshes": meshes,
        "accessors": accessors,
        "bufferViews": buffer_views,
        "buffers": [{"byteLength": bin.len()}],
    });
    let mut json_bytes = serde_json::to_vec(&document).expect("serialisable");
    while !json_bytes.len().is_multiple_of(4) {
        json_bytes.push(b' ');
    }
    while !bin.len().is_multiple_of(4) {
        bin.push(0);
    }

    let total = 12 + 8 + json_bytes.len() + 8 + bin.len();
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"glTF");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&(total as u32).to_le_bytes());
    out.extend_from_slice(&(json_bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(b"JSON");
    out.extend_from_slice(&json_bytes);
    out.extend_from_slice(&(bin.len() as u32).to_le_bytes());
    out.extend_from_slice(b"BIN\0");
    out.extend_from_slice(&bin);
    out
}

fn bounds(positions: &[[f32; 3]]) -> ([f32; 3], [f32; 3]) {
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for p in positions {
        for axis in 0..3 {
            min[axis] = min[axis].min(p[axis]);
            max[axis] = max[axis].max(p[axis]);
        }
    }
    (min, max)
}

/// The model as GLB bytes. `plinth_m` and `rail_height_m` come from the
/// parameters, not the model, which records only what the drawings show.
pub fn glb(model: &Model, plinth_m: f64, rail_height_m: f64) -> Vec<u8> {
    pack_glb(&layers(model, plinth_m, rail_height_m))
}

pub fn write_glb(path: &Path, model: &Model, plinth_m: f64, rail_height_m: f64) -> Result<()> {
    let mut file = std::fs::File::create(path)
        .with_context(|| format!("cannot write '{}'", path.display()))?;
    file.write_all(&glb(model, plinth_m, rail_height_m))?;
    file.flush()
        .with_context(|| format!("failed to flush '{}'", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square_with_hole() -> Polygon<f64> {
        Polygon::new(
            LineString::from(vec![
                (0.0, 0.0),
                (4.0, 0.0),
                (4.0, 4.0),
                (0.0, 4.0),
                (0.0, 0.0),
            ]),
            vec![LineString::from(vec![
                (1.0, 1.0),
                (1.0, 3.0),
                (3.0, 3.0),
                (3.0, 1.0),
                (1.0, 1.0),
            ])],
        )
    }

    #[test]
    fn an_extrusion_has_two_caps_and_a_wall_per_edge() {
        let mut layer = Layer::new("t");
        layer.extrude(&square_with_hole(), 0.0, 2.0, [235, 232, 225, 255]);
        // Caps: 8 vertices each, 8 triangles each (square with a hole).
        // Sides: 8 edges x 2 triangles.
        assert_eq!(layer.indices.len() / 3, 8 + 8 + 16);
        assert_eq!(layer.positions.len(), 16 + 32);
        assert!(layer.positions.iter().all(|p| p[2] == 0.0 || p[2] == 2.0));
    }

    #[test]
    fn the_glb_is_well_formed() {
        let mut layer = Layer::new("t");
        layer.extrude(&square_with_hole(), 0.0, 2.0, [235, 232, 225, 255]);
        let bytes = pack_glb(&[layer]);
        assert_eq!(&bytes[0..4], b"glTF");
        let total = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
        assert_eq!(total, bytes.len());
        let json_len = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
        assert_eq!(&bytes[16..20], b"JSON");
        let document: Value = serde_json::from_slice(&bytes[20..20 + json_len]).unwrap();
        assert_eq!(document["asset"]["version"], "2.0");
        assert_eq!(document["meshes"][0]["name"], "t");
        assert_eq!(document["accessors"].as_array().unwrap().len(), 3);
        let bin_start = 20 + json_len;
        assert_eq!(&bytes[bin_start + 4..bin_start + 8], b"BIN\0");
        assert_eq!(bytes.len() % 4, 0);
    }
}

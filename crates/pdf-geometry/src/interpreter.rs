//! A PDF content-stream interpreter that recovers painted vector geometry and
//! positioned text in page user space.
//!
//! Only the operators that carry geometry, colour or text are implemented;
//! anything else is consumed and ignored. Form XObjects are executed
//! recursively so that geometry nested behind a `cm`/`Do` pair — which is how
//! the CAD exports in scope place their drawings — lands in the same
//! coordinate space as the page.

use std::collections::HashMap;

use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Document, Object, ObjectId};

use super::cmap::ToUnicode;
use super::textrun::{MergeOptions, merge_runs};
use crate::model::{
    BBox, Matrix, PageContent, PaintMode, Point, Rgb, SubPath, TextItem, VectorPath,
};

/// Caps applied per page so that a pathological drawing cannot exhaust memory.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_content_bytes: usize,
    pub max_paths: usize,
    pub max_form_depth: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_content_bytes: 256 << 20,
            max_paths: 2_000_000,
            max_form_depth: 12,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ColorSpaceKind {
    Gray,
    Rgb,
    Cmyk,
    Other,
}

#[derive(Debug, Clone)]
struct GraphicsState {
    ctm: Matrix,
    stroke: Rgb,
    fill: Rgb,
    stroke_space: ColorSpaceKind,
    fill_space: ColorSpaceKind,
    line_width: f64,
    layer: Option<String>,
}

impl Default for GraphicsState {
    fn default() -> Self {
        Self {
            ctm: Matrix::IDENTITY,
            stroke: Rgb::BLACK,
            fill: Rgb::BLACK,
            stroke_space: ColorSpaceKind::Gray,
            fill_space: ColorSpaceKind::Gray,
            line_width: 1.0,
            layer: None,
        }
    }
}

#[derive(Debug, Clone, Default)]
struct TextState {
    matrix: Matrix,
    line_matrix: Matrix,
    font_size: f64,
    horizontal_scale: f64,
    char_spacing: f64,
    word_spacing: f64,
    rise: f64,
    leading: f64,
    font: Option<ToUnicode>,
    invisible: bool,
}

/// Extracts geometry and text from a page's content streams.
pub struct Interpreter<'a> {
    doc: &'a Document,
    limits: Limits,
    font_cache: HashMap<ObjectId, ToUnicode>,
    paths: Vec<VectorPath>,
    texts: Vec<TextItem>,
    image_count: usize,
    image_area: f64,
    truncated: bool,
}

impl<'a> Interpreter<'a> {
    pub fn new(doc: &'a Document, limits: Limits) -> Self {
        Self {
            doc,
            limits,
            font_cache: HashMap::new(),
            paths: Vec::new(),
            texts: Vec::new(),
            image_count: 0,
            image_area: 0.0,
            truncated: false,
        }
    }

    /// Runs the page's content streams and returns everything that was painted.
    ///
    /// `page_number` is 1-based and is copied into the result for provenance.
    pub fn run_page(
        mut self,
        page_id: ObjectId,
        page_number: usize,
    ) -> Result<PageContent, lopdf::Error> {
        let media_box = page_box(self.doc, page_id);
        let rotation = page_rotation(self.doc, page_id);
        let data = self
            .doc
            .get_page_content_with_limit(page_id, self.limits.max_content_bytes)
            .unwrap_or_default();
        let content = Content::decode(&data)?;

        let resources = page_resources(self.doc, page_id);
        // Fold `/Rotate` into the base transform so that extracted coordinates
        // are in *displayed* page space. Downstream code can then map a page
        // bounding box onto a rendered image without re-deriving the rotation.
        let base = Matrix::translate(-media_box.min_x, -media_box.min_y).then(&rotation_matrix(
            rotation,
            media_box.width(),
            media_box.height(),
        ));
        let mut state = GraphicsState {
            ctm: base,
            ..Default::default()
        };
        self.execute(&content.operations, &resources, &mut state, 0);

        let (page_w, page_h) = if rotation % 180 == 0 {
            (media_box.width(), media_box.height())
        } else {
            (media_box.height(), media_box.width())
        };
        let page = PageContent {
            page_number,
            media_box: BBox {
                min_x: 0.0,
                min_y: 0.0,
                max_x: page_w,
                max_y: page_h,
            },
            paths: self.paths,
            texts: merge_runs(self.texts, MergeOptions::default()),
            image_count: self.image_count,
            image_coverage: if media_box.area() > 0.0 {
                (self.image_area / media_box.area()).min(1.0)
            } else {
                0.0
            },
        };
        Ok(page)
    }

    /// Whether output was cut short by [`Limits::max_paths`].
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    fn execute(
        &mut self,
        ops: &[Operation],
        resources: &Dictionary,
        state: &mut GraphicsState,
        depth: usize,
    ) {
        let mut stack: Vec<GraphicsState> = Vec::new();
        let mut text = TextState::default();
        let mut current: Vec<SubPath> = Vec::new();
        let mut open: Vec<Point> = Vec::new();
        let mut start = Point::new(0.0, 0.0);
        let mut cursor = Point::new(0.0, 0.0);
        // `W`/`W*` set the clip from the *pending* path, which is only applied
        // at the next painting operator; the flag defers that discard.
        let mut pending_clip = false;
        let mut oc_stack: Vec<Option<String>> = Vec::new();

        macro_rules! flush_open {
            ($closed:expr) => {
                if open.len() >= 2 {
                    current.push(SubPath {
                        points: std::mem::take(&mut open),
                        closed: $closed,
                    });
                } else {
                    open.clear();
                }
            };
        }

        for op in ops {
            let o = op.operator.as_str();
            let a = &op.operands;
            match o {
                "q" => stack.push(state.clone()),
                "Q" => {
                    if let Some(prev) = stack.pop() {
                        *state = prev;
                    }
                }
                "cm" => {
                    if let Some(m) = matrix_from(a) {
                        state.ctm = m.then(&state.ctm);
                    }
                }
                "w" => state.line_width = num(a, 0).unwrap_or(state.line_width),
                "gs" => {
                    if let Some(lw) = ext_gstate_line_width(self.doc, resources, a) {
                        state.line_width = lw;
                    }
                }

                // ---- path construction ----
                "m" => {
                    flush_open!(false);
                    if let (Some(x), Some(y)) = (num(a, 0), num(a, 1)) {
                        cursor = Point::new(x, y);
                        start = cursor;
                        open.push(state.ctm.apply(cursor));
                    }
                }
                "l" => {
                    if let (Some(x), Some(y)) = (num(a, 0), num(a, 1)) {
                        cursor = Point::new(x, y);
                        open.push(state.ctm.apply(cursor));
                    }
                }
                "c" | "v" | "y" => {
                    let (c1, c2, end) = match (o, bezier_args(a)) {
                        ("c", Some(v)) => (
                            Point::new(v[0], v[1]),
                            Point::new(v[2], v[3]),
                            Point::new(v[4], v[5]),
                        ),
                        ("v", _) => {
                            let (Some(x2), Some(y2), Some(x3), Some(y3)) =
                                (num(a, 0), num(a, 1), num(a, 2), num(a, 3))
                            else {
                                continue;
                            };
                            (cursor, Point::new(x2, y2), Point::new(x3, y3))
                        }
                        ("y", _) => {
                            let (Some(x1), Some(y1), Some(x3), Some(y3)) =
                                (num(a, 0), num(a, 1), num(a, 2), num(a, 3))
                            else {
                                continue;
                            };
                            let end = Point::new(x3, y3);
                            (Point::new(x1, y1), end, end)
                        }
                        _ => continue,
                    };
                    flatten_cubic(&state.ctm, cursor, c1, c2, end, &mut open);
                    cursor = end;
                }
                "h" => {
                    if !open.is_empty() {
                        flush_open!(true);
                        cursor = start;
                    }
                }
                "re" => {
                    flush_open!(false);
                    let (Some(x), Some(y), Some(w), Some(h)) =
                        (num(a, 0), num(a, 1), num(a, 2), num(a, 3))
                    else {
                        continue;
                    };
                    let pts = [
                        Point::new(x, y),
                        Point::new(x + w, y),
                        Point::new(x + w, y + h),
                        Point::new(x, y + h),
                    ];
                    current.push(SubPath {
                        points: pts.iter().map(|p| state.ctm.apply(*p)).collect(),
                        closed: true,
                    });
                    cursor = pts[0];
                    start = cursor;
                }
                "W" | "W*" => pending_clip = true,

                // ---- path painting ----
                "S" | "s" | "f" | "F" | "f*" | "B" | "B*" | "b" | "b*" | "n" => {
                    let closes = matches!(o, "s" | "b" | "b*");
                    flush_open!(closes);
                    if o != "n" && !pending_clip {
                        let mode = match o {
                            "S" | "s" => PaintMode::Stroke,
                            "f" | "F" | "f*" => PaintMode::Fill,
                            _ => PaintMode::FillStroke,
                        };
                        self.emit_path(std::mem::take(&mut current), mode, state);
                    } else {
                        current.clear();
                    }
                    pending_clip = false;
                }

                // ---- colour ----
                "G" => {
                    state.stroke_space = ColorSpaceKind::Gray;
                    state.stroke = gray(a);
                }
                "g" => {
                    state.fill_space = ColorSpaceKind::Gray;
                    state.fill = gray(a);
                }
                "RG" => {
                    state.stroke_space = ColorSpaceKind::Rgb;
                    state.stroke = rgb(a);
                }
                "rg" => {
                    state.fill_space = ColorSpaceKind::Rgb;
                    state.fill = rgb(a);
                }
                "K" => {
                    state.stroke_space = ColorSpaceKind::Cmyk;
                    state.stroke = cmyk(a);
                }
                "k" => {
                    state.fill_space = ColorSpaceKind::Cmyk;
                    state.fill = cmyk(a);
                }
                "CS" => state.stroke_space = colorspace_kind(self.doc, resources, a),
                "cs" => state.fill_space = colorspace_kind(self.doc, resources, a),
                "SC" | "SCN" => state.stroke = components(a, state.stroke_space, state.stroke),
                "sc" | "scn" => state.fill = components(a, state.fill_space, state.fill),

                // ---- optional content (CAD layer names) ----
                "BDC" => {
                    oc_stack.push(state.layer.clone());
                    if let Some(name) = oc_name(a) {
                        state.layer = Some(name);
                    }
                }
                "BMC" => oc_stack.push(state.layer.clone()),
                "EMC" => {
                    if let Some(prev) = oc_stack.pop() {
                        state.layer = prev;
                    }
                }

                // ---- text ----
                "BT" => {
                    text.matrix = Matrix::IDENTITY;
                    text.line_matrix = Matrix::IDENTITY;
                    text.horizontal_scale = 1.0;
                }
                "ET" => text.font = None,
                "Tf" => {
                    text.font_size = num(a, 1).unwrap_or(text.font_size);
                    text.font = self.resolve_font(resources, a);
                }
                "Td" => {
                    if let (Some(tx), Some(ty)) = (num(a, 0), num(a, 1)) {
                        text.line_matrix = Matrix::translate(tx, ty).then(&text.line_matrix);
                        text.matrix = text.line_matrix;
                    }
                }
                "TD" => {
                    if let (Some(tx), Some(ty)) = (num(a, 0), num(a, 1)) {
                        text.leading = -ty;
                        text.line_matrix = Matrix::translate(tx, ty).then(&text.line_matrix);
                        text.matrix = text.line_matrix;
                    }
                }
                "Tm" => {
                    if let Some(m) = matrix_from(a) {
                        text.line_matrix = m;
                        text.matrix = m;
                    }
                }
                "T*" => {
                    text.line_matrix =
                        Matrix::translate(0.0, -text.leading).then(&text.line_matrix);
                    text.matrix = text.line_matrix;
                }
                "TL" => text.leading = num(a, 0).unwrap_or(text.leading),
                "Tc" => text.char_spacing = num(a, 0).unwrap_or(0.0),
                "Tw" => text.word_spacing = num(a, 0).unwrap_or(0.0),
                "Tz" => text.horizontal_scale = num(a, 0).unwrap_or(100.0) / 100.0,
                "Ts" => text.rise = num(a, 0).unwrap_or(0.0),
                "Tr" => text.invisible = num(a, 0).map(|m| m as i32 == 3).unwrap_or(false),
                "Tj" | "'" | "\"" => {
                    if matches!(o, "'" | "\"") {
                        text.line_matrix =
                            Matrix::translate(0.0, -text.leading).then(&text.line_matrix);
                        text.matrix = text.line_matrix;
                    }
                    if let Some(Object::String(bytes, _)) = a.last() {
                        self.show_text(bytes, &mut text, state);
                    }
                }
                "TJ" => {
                    let Some(Object::Array(items)) = a.first() else {
                        continue;
                    };
                    for item in items {
                        match item {
                            Object::String(bytes, _) => self.show_text(bytes, &mut text, state),
                            other => {
                                if let Ok(adj) = other.as_float() {
                                    let dx = -f64::from(adj) / 1000.0
                                        * text.font_size
                                        * text.horizontal_scale;
                                    text.matrix = Matrix::translate(dx, 0.0).then(&text.matrix);
                                }
                            }
                        }
                    }
                }

                "Do" => self.do_xobject(resources, a, state, depth),
                // Inline images carry no geometry worth recovering; count them so
                // page classification still sees raster content.
                "BI" => self.image_count += 1,
                "ID" | "EI" => {}
                _ => {}
            }

            if self.paths.len() >= self.limits.max_paths {
                self.truncated = true;
                return;
            }
        }
    }

    fn emit_path(&mut self, subpaths: Vec<SubPath>, mode: PaintMode, state: &GraphicsState) {
        if subpaths.is_empty() {
            return;
        }
        self.paths.push(VectorPath {
            subpaths,
            mode,
            stroke: state.stroke,
            fill: state.fill,
            width: state.line_width * state.ctm.mean_scale(),
            layer: state.layer.clone(),
        });
    }

    fn show_text(&mut self, bytes: &[u8], text: &mut TextState, state: &GraphicsState) {
        let decoded = match &text.font {
            Some(f) => f.decode(bytes),
            None => String::new(),
        };
        let scale = Matrix::new(
            text.font_size * text.horizontal_scale,
            0.0,
            0.0,
            text.font_size,
            0.0,
            text.rise,
        );
        let trm = scale.then(&text.matrix).then(&state.ctm);

        if !decoded.trim().is_empty() && !text.invisible {
            self.texts.push(TextItem {
                text: decoded.clone(),
                origin: trm.apply(Point::new(0.0, 0.0)),
                size: (text.font_size * text.matrix.mean_scale() * state.ctm.mean_scale()).abs(),
                angle: (trm.b).atan2(trm.a),
                color: state.fill,
            });
        }

        // Glyph widths are not tracked; runs in CAD exports are positioned by
        // their own `Tm`, so an average-width advance is sufficient to keep
        // multi-run lines from collapsing onto one point.
        let glyphs = decoded.chars().count().max(bytes.len() / 2) as f64;
        let advance = glyphs * (0.5 * text.font_size + text.char_spacing) * text.horizontal_scale;
        text.matrix = Matrix::translate(advance, 0.0).then(&text.matrix);
    }

    fn resolve_font(&mut self, resources: &Dictionary, operands: &[Object]) -> Option<ToUnicode> {
        let Some(Object::Name(name)) = operands.first() else {
            return None;
        };
        let fonts = resolve_dict(self.doc, resources, b"Font")?;
        let entry = fonts.get(name).ok()?;
        let id = entry.as_reference().ok();
        if let Some(id) = id {
            if let Some(cached) = self.font_cache.get(&id) {
                return Some(cached.clone());
            }
        }
        let dict = match entry {
            Object::Reference(r) => self.doc.get_object(*r).ok()?.as_dict().ok()?,
            Object::Dictionary(d) => d,
            _ => return None,
        };
        let map = self.load_to_unicode(dict);
        if let Some(id) = id {
            self.font_cache.insert(id, map.clone());
        }
        Some(map)
    }

    fn load_to_unicode(&self, font: &Dictionary) -> ToUnicode {
        let two_byte = font
            .get(b"Encoding")
            .and_then(Object::as_name)
            .map(|n| n.starts_with(b"Identity"))
            .unwrap_or_else(|_| font.get(b"DescendantFonts").is_ok());
        let fallback = ToUnicode::identity(if two_byte { 2 } else { 1 });

        let Ok(tu) = font.get(b"ToUnicode") else {
            return fallback;
        };
        let stream = match tu {
            Object::Reference(r) => self
                .doc
                .get_object(*r)
                .ok()
                .and_then(|o| o.as_stream().ok()),
            Object::Stream(s) => Some(s),
            _ => None,
        };
        let Some(stream) = stream else {
            return fallback;
        };
        let Ok(data) = stream.decompressed_content() else {
            return fallback;
        };
        let mut map = ToUnicode::parse(&data);
        if map.is_empty() {
            return fallback;
        }
        map.code_bytes = if two_byte { 2 } else { 1 };
        map
    }

    fn do_xobject(
        &mut self,
        resources: &Dictionary,
        operands: &[Object],
        state: &GraphicsState,
        depth: usize,
    ) {
        let Some(Object::Name(name)) = operands.first() else {
            return;
        };
        let Some(xobjects) = resolve_dict(self.doc, resources, b"XObject") else {
            return;
        };
        let Ok(entry) = xobjects.get(name) else {
            return;
        };
        let Ok(id) = entry.as_reference() else { return };
        let Ok(stream) = self.doc.get_object(id).and_then(Object::as_stream) else {
            return;
        };

        let subtype = stream
            .dict
            .get(b"Subtype")
            .and_then(Object::as_name)
            .unwrap_or(b"");
        if subtype == b"Image" {
            self.image_count += 1;
            // A `Do` on an image maps the unit square through the CTM, so the
            // parallelogram's area is |a·d − b·c|.
            let m = state.ctm;
            self.image_area += (m.a * m.d - m.b * m.c).abs();
            return;
        }
        if subtype != b"Form" || depth >= self.limits.max_form_depth {
            return;
        }

        let form_matrix = stream
            .dict
            .get(b"Matrix")
            .and_then(Object::as_array)
            .ok()
            .and_then(|arr| matrix_from(arr))
            .unwrap_or(Matrix::IDENTITY);
        let form_resources = stream
            .dict
            .get(b"Resources")
            .ok()
            .and_then(|o| deref_dict(self.doc, o))
            .unwrap_or_else(|| resources.clone());

        let Ok(data) = stream.decompressed_content() else {
            return;
        };
        let Ok(content) = Content::decode(&data) else {
            return;
        };
        let mut inner = state.clone();
        inner.ctm = form_matrix.then(&state.ctm);
        self.execute(&content.operations, &form_resources, &mut inner, depth + 1);
    }
}

// --------------------------------------------------------------------------
// operand helpers
// --------------------------------------------------------------------------

fn num(operands: &[Object], i: usize) -> Option<f64> {
    operands
        .get(i)
        .and_then(|o| o.as_float().ok())
        .map(f64::from)
}

fn matrix_from(operands: &[Object]) -> Option<Matrix> {
    if operands.len() < 6 {
        return None;
    }
    Some(Matrix::new(
        num(operands, 0)?,
        num(operands, 1)?,
        num(operands, 2)?,
        num(operands, 3)?,
        num(operands, 4)?,
        num(operands, 5)?,
    ))
}

fn bezier_args(operands: &[Object]) -> Option<[f64; 6]> {
    if operands.len() < 6 {
        return None;
    }
    Some([
        num(operands, 0)?,
        num(operands, 1)?,
        num(operands, 2)?,
        num(operands, 3)?,
        num(operands, 4)?,
        num(operands, 5)?,
    ])
}

fn gray(operands: &[Object]) -> Rgb {
    let v = num(operands, 0).unwrap_or(0.0);
    Rgb { r: v, g: v, b: v }
}

fn rgb(operands: &[Object]) -> Rgb {
    Rgb {
        r: num(operands, 0).unwrap_or(0.0),
        g: num(operands, 1).unwrap_or(0.0),
        b: num(operands, 2).unwrap_or(0.0),
    }
}

fn cmyk(operands: &[Object]) -> Rgb {
    let (c, m, y, k) = (
        num(operands, 0).unwrap_or(0.0),
        num(operands, 1).unwrap_or(0.0),
        num(operands, 2).unwrap_or(0.0),
        num(operands, 3).unwrap_or(0.0),
    );
    Rgb {
        r: (1.0 - c) * (1.0 - k),
        g: (1.0 - m) * (1.0 - k),
        b: (1.0 - y) * (1.0 - k),
    }
}

/// Resolves `SC`/`SCN` operands to an RGB colour.
///
/// Three or four components are unambiguous whatever the declared space. A
/// single component is not: in `DeviceGray` it is a lightness, but in a
/// Separation space it is an ink tint, where the same number means the opposite
/// shade. Rather than guess, a lone component outside a grey space keeps the
/// previous colour — as do pattern colours, which carry a trailing name and no
/// usable components at all.
fn components(operands: &[Object], space: ColorSpaceKind, previous: Rgb) -> Rgb {
    let numeric: Vec<f64> = operands
        .iter()
        .filter_map(|o| o.as_float().ok())
        .map(f64::from)
        .collect();
    match (space, numeric.len()) {
        (_, 3) => Rgb {
            r: numeric[0],
            g: numeric[1],
            b: numeric[2],
        },
        (_, 4) => cmyk(operands),
        (ColorSpaceKind::Gray, 1) => Rgb {
            r: numeric[0],
            g: numeric[0],
            b: numeric[0],
        },
        _ => previous,
    }
}

fn colorspace_kind(doc: &Document, resources: &Dictionary, operands: &[Object]) -> ColorSpaceKind {
    let Some(Object::Name(name)) = operands.first() else {
        return ColorSpaceKind::Other;
    };
    match name.as_slice() {
        b"DeviceGray" | b"G" | b"CalGray" => return ColorSpaceKind::Gray,
        b"DeviceRGB" | b"RGB" | b"CalRGB" => return ColorSpaceKind::Rgb,
        b"DeviceCMYK" | b"CMYK" => return ColorSpaceKind::Cmyk,
        _ => {}
    }
    // Named spaces resolve through /ColorSpace; only the device families matter
    // for recovering a usable line colour.
    let Some(spaces) = resolve_dict(doc, resources, b"ColorSpace") else {
        return ColorSpaceKind::Other;
    };
    let Ok(entry) = spaces.get(name) else {
        return ColorSpaceKind::Other;
    };
    let resolved = match entry {
        Object::Reference(r) => doc.get_object(*r).ok(),
        other => Some(other),
    };
    match resolved {
        Some(Object::Name(n)) if n == b"DeviceRGB" => ColorSpaceKind::Rgb,
        Some(Object::Name(n)) if n == b"DeviceGray" => ColorSpaceKind::Gray,
        Some(Object::Name(n)) if n == b"DeviceCMYK" => ColorSpaceKind::Cmyk,
        _ => ColorSpaceKind::Other,
    }
}

fn oc_name(operands: &[Object]) -> Option<String> {
    match operands.first() {
        Some(Object::Name(tag)) if tag == b"OC" => match operands.get(1) {
            Some(Object::Name(n)) => Some(String::from_utf8_lossy(n).into_owned()),
            _ => None,
        },
        _ => None,
    }
}

fn ext_gstate_line_width(
    doc: &Document,
    resources: &Dictionary,
    operands: &[Object],
) -> Option<f64> {
    let Some(Object::Name(name)) = operands.first() else {
        return None;
    };
    let states = resolve_dict(doc, resources, b"ExtGState")?;
    let entry = states.get(name).ok()?;
    let dict = match entry {
        Object::Reference(r) => doc.get_object(*r).ok()?.as_dict().ok()?,
        Object::Dictionary(d) => d,
        _ => return None,
    };
    dict.get(b"LW")
        .and_then(Object::as_float)
        .ok()
        .map(f64::from)
}

fn deref_dict(doc: &Document, obj: &Object) -> Option<Dictionary> {
    match obj {
        Object::Dictionary(d) => Some(d.clone()),
        Object::Reference(r) => doc.get_object(*r).ok()?.as_dict().ok().cloned(),
        _ => None,
    }
}

fn resolve_dict(doc: &Document, resources: &Dictionary, key: &[u8]) -> Option<Dictionary> {
    resources.get(key).ok().and_then(|o| deref_dict(doc, o))
}

/// Collects the page's own `/Resources` plus any inherited from its ancestors,
/// with the nearest definition winning.
fn page_resources(doc: &Document, page_id: ObjectId) -> Dictionary {
    let mut merged = Dictionary::new();
    let Ok((own, inherited_ids)) = doc.get_page_resources(page_id) else {
        return merged;
    };
    for id in inherited_ids.into_iter().rev() {
        if let Ok(d) = doc.get_dictionary(id) {
            for (k, v) in d.iter() {
                merged.set(k.clone(), v.clone());
            }
        }
    }
    if let Some(d) = own {
        for (k, v) in d.iter() {
            merged.set(k.clone(), v.clone());
        }
    }
    merged
}

/// Normalised `/Rotate` value in degrees (0, 90, 180 or 270), inherited from
/// the page tree if not set on the page itself.
fn page_rotation(doc: &Document, page_id: ObjectId) -> i64 {
    let mut node = match doc.get_dictionary(page_id) {
        Ok(d) => d,
        Err(_) => return 0,
    };
    for _ in 0..16 {
        if let Ok(r) = node.get(b"Rotate").and_then(Object::as_i64) {
            return r.rem_euclid(360) / 90 * 90;
        }
        let Ok(parent) = node.get(b"Parent").and_then(Object::as_reference) else {
            return 0;
        };
        match doc.get_dictionary(parent) {
            Ok(d) => node = d,
            Err(_) => return 0,
        }
    }
    0
}

/// Rotates a `w x h` page (origin already at 0,0) clockwise by `degrees`,
/// keeping the result in the positive quadrant.
fn rotation_matrix(degrees: i64, w: f64, h: f64) -> Matrix {
    match degrees {
        90 => Matrix::new(0.0, -1.0, 1.0, 0.0, 0.0, w),
        180 => Matrix::new(-1.0, 0.0, 0.0, -1.0, w, h),
        270 => Matrix::new(0.0, 1.0, -1.0, 0.0, h, 0.0),
        _ => Matrix::IDENTITY,
    }
}

/// The page's crop box if present, else its media box, else US Letter.
fn page_box(doc: &Document, page_id: ObjectId) -> BBox {
    let fallback = BBox {
        min_x: 0.0,
        min_y: 0.0,
        max_x: 612.0,
        max_y: 792.0,
    };
    let Ok(dict) = doc.get_dictionary(page_id) else {
        return fallback;
    };

    let read = |key: &[u8]| -> Option<BBox> {
        let mut node = dict;
        let mut hops = 0;
        loop {
            if let Ok(arr) = node.get(key).and_then(Object::as_array) {
                let v: Vec<f64> = arr
                    .iter()
                    .filter_map(|o| o.as_float().ok())
                    .map(f64::from)
                    .collect();
                if v.len() == 4 {
                    return Some(BBox {
                        min_x: v[0].min(v[2]),
                        min_y: v[1].min(v[3]),
                        max_x: v[0].max(v[2]),
                        max_y: v[1].max(v[3]),
                    });
                }
            }
            hops += 1;
            if hops > 16 {
                return None;
            }
            let parent = node.get(b"Parent").and_then(Object::as_reference).ok()?;
            node = doc.get_dictionary(parent).ok()?;
        }
    };
    read(b"CropBox")
        .or_else(|| read(b"MediaBox"))
        .unwrap_or(fallback)
}

/// Recursively subdivides a cubic Bézier until each span is flat to within
/// `TOLERANCE` user-space units, appending the transformed points to `out`.
fn flatten_cubic(ctm: &Matrix, p0: Point, p1: Point, p2: Point, p3: Point, out: &mut Vec<Point>) {
    const TOLERANCE: f64 = 0.05;
    const MAX_DEPTH: u8 = 8;

    fn recurse(p0: Point, p1: Point, p2: Point, p3: Point, out: &mut Vec<Point>, depth: u8) {
        let dx = p3.x - p0.x;
        let dy = p3.y - p0.y;
        let d1 = ((p1.x - p3.x) * dy - (p1.y - p3.y) * dx).abs();
        let d2 = ((p2.x - p3.x) * dy - (p2.y - p3.y) * dx).abs();
        let flat = (d1 + d2).powi(2) <= TOLERANCE * (dx * dx + dy * dy);
        if depth >= MAX_DEPTH || flat {
            out.push(p3);
            return;
        }
        let mid = |a: Point, b: Point| Point::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0);
        let (p01, p12, p23) = (mid(p0, p1), mid(p1, p2), mid(p2, p3));
        let (p012, p123) = (mid(p01, p12), mid(p12, p23));
        let mid_point = mid(p012, p123);
        recurse(p0, p01, p012, mid_point, out, depth + 1);
        recurse(mid_point, p123, p23, p3, out, depth + 1);
    }

    // Flatten in device space so the tolerance is meaningful regardless of the
    // scale factor baked into the CTM.
    let (d0, d1, d2, d3) = (ctm.apply(p0), ctm.apply(p1), ctm.apply(p2), ctm.apply(p3));
    if out.is_empty() {
        out.push(d0);
    }
    recurse(d0, d1, d2, d3, out, 0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matrix_composition_matches_pdf_order() {
        let scale = Matrix::scale(2.0, 2.0);
        let translate = Matrix::translate(10.0, 0.0);
        // `cm` semantics: the new matrix applies first, then the existing CTM.
        let combined = scale.then(&translate);
        assert_eq!(combined.apply(Point::new(1.0, 1.0)), Point::new(12.0, 2.0));
    }

    #[test]
    fn cmyk_black_is_black() {
        let c = cmyk(&[
            Object::Real(0.0),
            Object::Real(0.0),
            Object::Real(0.0),
            Object::Real(1.0),
        ]);
        assert!(c.luminance() < 0.01);
    }

    #[test]
    fn flatten_emits_endpoint() {
        let mut out = Vec::new();
        flatten_cubic(
            &Matrix::IDENTITY,
            Point::new(0.0, 0.0),
            Point::new(0.0, 10.0),
            Point::new(10.0, 10.0),
            Point::new(10.0, 0.0),
            &mut out,
        );
        assert_eq!(out.first().copied(), Some(Point::new(0.0, 0.0)));
        assert_eq!(out.last().copied(), Some(Point::new(10.0, 0.0)));
        assert!(out.len() > 4, "curve should subdivide");
    }
}

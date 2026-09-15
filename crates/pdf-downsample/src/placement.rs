//! Where each image XObject is painted, and how large.
//!
//! An image's resolution is not a property of the image: the same 3000 px
//! photo is 1000 dpi on a thumbnail and 100 dpi across a poster sheet. The
//! only honest measure is pixels over the size it is *drawn* at, which is the
//! CTM in force at its `Do`. So the content streams are walked — only `q`,
//! `Q`, `cm` and `Do`, recursing into form XObjects — and every placement is
//! recorded. `pdf-geometry`'s interpreter tracks the same CTM, but it also
//! flattens every path and decodes every string, which on a CAD sheet is the
//! whole cost of reading it and none of what is needed here.
//!
//! An image drawn at several sizes keeps enough pixels for the largest, and
//! one that is never reached this way (a pattern fill, an annotation
//! appearance, a stream this walker could not parse) is simply absent: an
//! image whose display size is unknown is never resampled.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::rc::Rc;

use lopdf::content::Content;
use lopdf::{Dictionary, Document, Object, ObjectId};
use pdf_geometry::{Limits, Matrix, matrix_from, page_resources};

/// A placement smaller than this (in inches, per axis) is a degenerate CTM,
/// not a real drawing size, and would demand no pixels at all.
const MIN_DISPLAY_INCHES: f64 = 1e-3;

/// The largest size an image is drawn at, over every page that draws it.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Placement {
    /// Displayed extent of the image's width axis, in inches.
    pub inches_x: f64,
    /// Displayed extent of the image's height axis, in inches.
    pub inches_y: f64,
    /// One-based pages that paint the image.
    pub pages: BTreeSet<u32>,
}

impl Placement {
    /// Grows to cover a drawing this large on these pages.
    fn widen(&mut self, inches_x: f64, inches_y: f64, pages: impl IntoIterator<Item = u32>) {
        self.inches_x = self.inches_x.max(inches_x);
        self.inches_y = self.inches_y.max(inches_y);
        self.pages.extend(pages);
    }
}

/// Every image XObject reachable from a page's content, with its largest
/// placement, plus the pages whose content could not be read.
#[derive(Debug, Default)]
pub(crate) struct Placements {
    pub images: BTreeMap<ObjectId, Placement>,
    pub unreadable_pages: Vec<u32>,
    /// Images reachable from content that could not be walked. Their largest
    /// placement is unknown even when another page draws them, so they are
    /// dropped from `images` once the walk is done.
    unknown: BTreeSet<ObjectId>,
}

/// Walks every page of `doc`.
///
/// A soft mask is sized with its image: it is not painted by a `Do` of its
/// own, but it covers exactly the area its parent does.
pub(crate) fn collect(doc: &Document) -> Placements {
    let limits = Limits::default();
    let mut out = Placements::default();
    // A logo or title block is one form drawn on every sheet: parse it once.
    let mut forms = HashMap::new();
    for (page_number, page_id) in doc.get_pages() {
        let user_unit = doc
            .get_dictionary(page_id)
            .ok()
            .and_then(|page| page.get(b"UserUnit").ok())
            .and_then(|unit| unit.as_float().ok())
            .map(f64::from)
            .filter(|unit| unit.is_finite() && *unit > 0.0)
            .unwrap_or(1.0);
        let content = doc
            .get_page_content_with_limit(page_id, limits.max_content_bytes)
            .ok()
            .and_then(|data| Content::decode(&data).ok());
        let resources = page_resources(doc, page_id);
        let Some(content) = content else {
            out.unreadable_pages.push(page_number);
            mark_unknown(doc, &resources, &mut out.unknown);
            continue;
        };
        let mut walker = Walker {
            doc,
            page_number,
            scale: user_unit / 72.0,
            max_depth: limits.max_form_depth,
            out: &mut out,
            forms: &mut forms,
            stack: Vec::new(),
        };
        walker.run(&content, &resources, Matrix::IDENTITY);
    }
    attach_soft_masks(doc, &mut out.images);
    let unknown = std::mem::take(&mut out.unknown);
    out.images.retain(|id, _| !unknown.contains(id));
    out
}

/// Records every image reachable from `resources`, through nested forms, and
/// its soft mask, as drawn at an unknown size.
fn mark_unknown<'d>(
    doc: &'d Document,
    resources: &'d Dictionary,
    unknown: &mut BTreeSet<ObjectId>,
) {
    let mut pending = vec![resources];
    let mut forms = BTreeSet::new();
    while let Some(resources) = pending.pop() {
        let Some(xobjects) = dictionary(doc, resources, b"XObject") else {
            continue;
        };
        for (_, value) in xobjects.iter() {
            let Ok(id) = value.as_reference() else {
                continue;
            };
            let Ok(stream) = doc.get_object(id).and_then(Object::as_stream) else {
                continue;
            };
            match stream.dict.get(b"Subtype").and_then(Object::as_name) {
                Ok(b"Image") => {
                    unknown.insert(id);
                    if let Ok(mask) = stream.dict.get(b"SMask").and_then(Object::as_reference) {
                        unknown.insert(mask);
                    }
                }
                Ok(b"Form") if forms.insert(id) => {
                    pending.push(dictionary(doc, &stream.dict, b"Resources").unwrap_or(resources));
                }
                _ => {}
            }
        }
    }
}

struct Walker<'a> {
    doc: &'a Document,
    page_number: u32,
    /// User-space units to inches.
    scale: f64,
    max_depth: usize,
    out: &'a mut Placements,
    /// Parsed form content by object id; `None` for a form that did not parse.
    forms: &'a mut HashMap<ObjectId, Option<Rc<Content>>>,
    /// Forms being executed, outermost first; a form already on it is a cycle.
    stack: Vec<ObjectId>,
}

impl<'a> Walker<'a> {
    fn run(&mut self, content: &Content, resources: &Dictionary, base: Matrix) {
        let mut ctm = base;
        let mut saved = Vec::new();
        for op in &content.operations {
            match op.operator.as_str() {
                "q" => saved.push(ctm),
                "Q" => ctm = saved.pop().unwrap_or(base),
                "cm" => {
                    if let Some(m) = matrix_from(&op.operands) {
                        ctm = m.then(&ctm);
                    }
                }
                "Do" => {
                    if let Some(Object::Name(name)) = op.operands.first() {
                        self.paint(name, resources, ctm);
                    }
                }
                _ => {}
            }
        }
    }

    fn paint(&mut self, name: &[u8], resources: &Dictionary, ctm: Matrix) {
        let doc = self.doc;
        let Some(id) = dictionary(doc, resources, b"XObject")
            .and_then(|xobjects| xobjects.get(name).and_then(Object::as_reference).ok())
        else {
            return;
        };
        let Ok(stream) = doc.get_object(id).and_then(Object::as_stream) else {
            return;
        };
        match stream.dict.get(b"Subtype").and_then(Object::as_name) {
            Ok(b"Image") => self.record(id, ctm),
            Ok(b"Form") if !self.stack.contains(&id) => {
                let form_resources =
                    dictionary(doc, &stream.dict, b"Resources").unwrap_or(resources);
                if self.stack.len() >= self.max_depth {
                    mark_unknown(doc, form_resources, &mut self.out.unknown);
                    return;
                }
                let Some(content) = self
                    .forms
                    .entry(id)
                    .or_insert_with(|| {
                        stream
                            .decompressed_content()
                            .ok()
                            .and_then(|data| Content::decode(&data).ok())
                            .map(Rc::new)
                    })
                    .clone()
                else {
                    mark_unknown(doc, form_resources, &mut self.out.unknown);
                    return;
                };
                let form_matrix = stream
                    .dict
                    .get(b"Matrix")
                    .and_then(Object::as_array)
                    .ok()
                    .and_then(|values| matrix_from(values))
                    .unwrap_or(Matrix::IDENTITY);
                self.stack.push(id);
                self.run(&content, form_resources, form_matrix.then(&ctm));
                self.stack.pop();
            }
            _ => {}
        }
    }

    /// A `Do` maps the image's unit square through the CTM, so the drawn
    /// width is the length of the CTM's first row and the height the second.
    fn record(&mut self, id: ObjectId, ctm: Matrix) {
        let inches_x = ctm.a.hypot(ctm.b) * self.scale;
        let inches_y = ctm.c.hypot(ctm.d) * self.scale;
        if inches_x >= MIN_DISPLAY_INCHES && inches_y >= MIN_DISPLAY_INCHES {
            let page = self.page_number;
            self.out
                .images
                .entry(id)
                .or_default()
                .widen(inches_x, inches_y, [page]);
        }
    }
}

fn attach_soft_masks(doc: &Document, images: &mut BTreeMap<ObjectId, Placement>) {
    let masks: Vec<(ObjectId, f64, f64, Vec<u32>)> = images
        .iter()
        .filter_map(|(id, p)| {
            let stream = doc.get_object(*id).and_then(Object::as_stream).ok()?;
            let mask = stream
                .dict
                .get(b"SMask")
                .and_then(Object::as_reference)
                .ok()?;
            Some((
                mask,
                p.inches_x,
                p.inches_y,
                p.pages.iter().copied().collect(),
            ))
        })
        .collect();
    for (mask, inches_x, inches_y, pages) in masks {
        images
            .entry(mask)
            .or_default()
            .widen(inches_x, inches_y, pages);
    }
}

/// A dictionary entry that is a dictionary, directly or by reference, borrowed
/// rather than copied: a page can list thousands of XObjects, and cloning that
/// map on every `Do` made the walk quadratic.
fn dictionary<'d>(doc: &'d Document, parent: &'d Dictionary, key: &[u8]) -> Option<&'d Dictionary> {
    match parent.get(key).ok()? {
        Object::Dictionary(dict) => Some(dict),
        Object::Reference(id) => doc.get_dictionary(*id).ok(),
        _ => None,
    }
}

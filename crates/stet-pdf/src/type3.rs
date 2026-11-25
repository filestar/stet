// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! PostScript Type 3 text kept as text, in PDF Type 3 fonts.
//!
//! A Type 3 glyph is whatever its `BuildChar` or `BuildGlyph` procedure
//! paints. The interpreter puts a `Text` element in front of those marks
//! ([`Type3Glyph`]); here they are mapped back into glyph space and become
//! the glyph's procedure in a PDF Type 3 font (`/CharProcs`), and the text is
//! shown with `Tj` like any other. The text can then be searched and copied,
//! and a PDF-to-PostScript step can write it back as a Type 3 font.
//!
//! What a PDF glyph procedure can hold here is deliberately narrow: filled
//! paths, strokes, stencil masks (bitmap fonts) and, in a glyph that sets
//! its own colours (`setcharwidth`, `d0`), small images. A glyph with
//! anything else — a shading, a pattern, a clip, a group, transparency,
//! overprint, a spot or ICC colour, or text of another font — is drawn as
//! the marks it made, as before; only that glyph, not its font.
//!
//! One PDF font holds one procedure per code. Showings of a font that draw
//! a code differently (a procedure that draws by size or colour, or two
//! fonts with one name) go to further PDF fonts of the same name.

use std::collections::{BTreeMap, HashMap};
use std::io::Write as IoWrite;

use stet_fonts::geometry::{Matrix, PathSegment, PsPath};
use stet_graphics::color::{DeviceColor, FillRule, LineCap, LineJoin};
use stet_graphics::device::{ImageColorSpace, ImageParams, StrokeParams, TextParams, Type3Glyph};
use stet_graphics::display_list::DisplayElement;

use crate::content_stream::{compute_image_matrix, emit_path, fmt_num, is_identity};
use crate::image_ops;
use crate::pdf_objects::PdfObj;
use crate::pdf_writer::PdfWriter;

/// The largest image, in bytes of samples, a glyph procedure keeps inline.
/// Bitmap glyphs are far smaller; a larger image stays on the page.
const MAX_INLINE_IMAGE: usize = 64 * 1024;

/// The PDF Type 3 fonts a document's PostScript Type 3 text is shown in.
#[derive(Default)]
pub struct Type3Fonts {
    fonts: Vec<Type3Font>,
    /// PostScript font name → indices of its PDF fonts in `fonts`.
    by_name: HashMap<Vec<u8>, Vec<usize>>,
}

/// One PDF Type 3 font.
struct Type3Font {
    /// The font's resource name (`T0`, `T1`, …).
    pdf_name: String,
    /// The PostScript `FontName`, for `/Name` and the ToUnicode CMap.
    font_name: Vec<u8>,
    /// The PDF `/FontMatrix`: the first showing's PostScript `FontMatrix`.
    /// Every glyph procedure is in glyph space, so a showing at another
    /// size only needs its own text matrix.
    font_matrix: Matrix,
    /// The inverse of `font_matrix`.
    font_matrix_inv: Matrix,
    /// The glyph each code draws.
    glyphs: BTreeMap<u8, CharProc>,
}

/// One glyph procedure of a PDF Type 3 font.
struct CharProc {
    /// The PostScript glyph name, when the font's `Encoding` gives one.
    glyph_name: Option<Vec<u8>>,
    /// The procedure: `d0` or `d1`, then the marks in glyph space.
    content: Vec<u8>,
    /// The advance width, in glyph space.
    width: f64,
    /// The marks' box in glyph space, `None` when there are none.
    bbox: Option<[f64; 4]>,
}

/// Where a glyph went: the font to show it in, and the text matrix that
/// places that font's glyph space on the page.
pub struct Placed<'a> {
    pub pdf_name: &'a str,
    pub text_matrix: Matrix,
}

impl Type3Fonts {
    /// Add one Type 3 glyph, drawn by `marks`, to a PDF font: one of its
    /// font's that has no glyph at the code yet, or draws it the same, else
    /// a new one. `None` when the glyph cannot be kept as text: its marks
    /// are then drawn as they are.
    pub fn place(
        &mut self,
        text: &TextParams,
        glyph: &Type3Glyph,
        marks: &[DisplayElement],
    ) -> Option<Placed<'_>> {
        if !plain_text(text) {
            return None;
        }
        let code = *text.text.first()?;
        let proc = char_proc(glyph, marks)?;

        let group = self.by_name.entry(text.font_name.clone()).or_default();
        let found = group.iter().copied().find(|&i| {
            self.fonts[i]
                .glyphs
                .get(&code)
                .is_none_or(|g| g.content == proc.content && g.width == proc.width)
        });
        let idx = match found {
            Some(i) => i,
            None => {
                let font_matrix_inv = glyph.font_matrix.invert()?;
                let idx = self.fonts.len();
                self.fonts.push(Type3Font {
                    pdf_name: format!("T{idx}"),
                    font_name: text.font_name.clone(),
                    font_matrix: glyph.font_matrix,
                    font_matrix_inv,
                    glyphs: BTreeMap::new(),
                });
                group.push(idx);
                idx
            }
        };
        let font = &mut self.fonts[idx];
        font.glyphs.entry(code).or_insert(proc);
        // Text space is glyph space through the FontMatrix: the text matrix
        // takes it back to glyph space, then on to the device.
        let text_matrix = glyph.glyph_to_device.multiply(&font.font_matrix_inv);
        Some(Placed {
            pdf_name: &font.pdf_name,
            text_matrix,
        })
    }

    /// Write every font, and return each one's resource name and object.
    pub fn write(&self, writer: &mut PdfWriter) -> Vec<(String, u32)> {
        self.fonts
            .iter()
            .map(|font| (font.pdf_name.clone(), write_font(writer, font)))
            .collect()
    }
}

/// Whether the text's paint is one a glyph shown in a Type 3 font keeps:
/// a plain device colour, opaque, filled.
fn plain_text(text: &TextParams) -> bool {
    text.paint_type == 0
        && text.spot_color.is_none()
        && text.icc_color.is_none()
        && text.fill_opacity == 1.0
        && text.blend_mode == 0
        && text.text.len() == 1
}

/// The glyph procedure for `marks`, in glyph space, or `None` when one of
/// them cannot be in one.
fn char_proc(glyph: &Type3Glyph, marks: &[DisplayElement]) -> Option<CharProc> {
    let g = glyph.glyph_to_device;
    let to_glyph = g.invert()?;
    // Strokes keep their device-space pen: their path is moved to the
    // glyph's origin and drawn under the inverse of the glyph's linear map.
    let linear_inv = Matrix::new(g.a, g.b, g.c, g.d, 0.0, 0.0).invert()?;
    let to_origin = Matrix::translate(-g.tx, -g.ty);

    let mut body = Vec::new();
    let mut bounds = Bounds::default();
    let colored = !glyph.stencil;
    for mark in marks {
        match mark {
            DisplayElement::Fill { path, params } => {
                if params.is_text_glyph
                    || params.overprint
                    || params.alpha != 1.0
                    || params.blend_mode != 0
                    || (colored && (params.spot_color.is_some() || params.icc_color.is_some()))
                {
                    return None;
                }
                if colored {
                    emit_fill_color(&mut body, &params.color);
                }
                let path = path.transform(&to_glyph);
                bounds.add_path(&path, 0.0);
                emit_path(&mut body, &path);
                body.extend(match params.fill_rule {
                    FillRule::EvenOdd => b"f*\n".as_slice(),
                    _ => b"f\n".as_slice(),
                });
            }
            DisplayElement::Stroke { path, params } => {
                if params.is_text_glyph
                    || params.overprint
                    || params.alpha != 1.0
                    || params.blend_mode != 0
                    || !is_identity(&params.ctm)
                    || (colored && (params.spot_color.is_some() || params.icc_color.is_some()))
                {
                    return None;
                }
                let local = path.transform(&to_origin);
                // Half the pen, in glyph space, around the glyph-space path.
                let half_pen = 0.5 * params.line_width * linear_inv.determinant().abs().sqrt();
                bounds.add_path(&local.transform(&linear_inv), half_pen);
                body.extend(b"q\n");
                emit_cm(&mut body, &linear_inv);
                if colored {
                    emit_stroke_color(&mut body, &params.color);
                }
                emit_line_state(&mut body, params);
                emit_path(&mut body, &local);
                body.extend(b"S\nQ\n");
            }
            DisplayElement::Image {
                sample_data,
                params,
            } => {
                let image = inline_image(sample_data, params, colored)?;
                let m = to_glyph.multiply(&compute_image_matrix(params));
                for (u, v) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)] {
                    bounds.add(m.transform_point(u, v), 0.0);
                }
                body.extend(b"q\n");
                emit_cm(&mut body, &m);
                body.extend(image);
                body.extend(b"Q\n");
            }
            _ => return None,
        }
    }

    let (wx, wy) = glyph.width;
    let mut content = Vec::with_capacity(body.len() + 48);
    if glyph.stencil {
        // `d1` declares the glyph a stencil in the text's colour, inside
        // its box: setcachedevice's, else the marks'.
        let bbox = glyph.bbox.or(bounds.rect()).unwrap_or([0.0; 4]);
        for v in [wx, wy, bbox[0], bbox[1], bbox[2], bbox[3]] {
            fmt_num(&mut content, v);
            content.push(b' ');
        }
        content.extend(b"d1\n");
    } else {
        fmt_num(&mut content, wx);
        content.push(b' ');
        fmt_num(&mut content, wy);
        content.extend(b" d0\n");
    }
    content.extend(body);
    Some(CharProc {
        glyph_name: glyph.glyph_name.clone(),
        content,
        width: wx,
        bbox: glyph.bbox.or(bounds.rect()),
    })
}

/// An image as an inline image (`BI … ID … EI`), its samples in ASCIIHex so
/// no byte of them can end it early. A stencil mask in any glyph; an 8-bit
/// Gray, RGB or CMYK image only in a glyph with colours of its own.
fn inline_image(sample_data: &[u8], params: &ImageParams, colored: bool) -> Option<Vec<u8>> {
    if sample_data.len() > MAX_INLINE_IMAGE
        || params.alpha != 1.0
        || params.blend_mode != 0
        || params.overprint
        || params.mask_color.is_some()
    {
        return None;
    }
    let mut out = Vec::new();
    let data = match &params.color_space {
        ImageColorSpace::Mask {
            color, spot_color, ..
        } => {
            if spot_color.is_some() {
                return None;
            }
            if colored {
                emit_fill_color(&mut out, color);
            }
            let xobj = image_ops::convert_image(sample_data, params);
            writeln!(
                out,
                "BI /IM true /W {} /H {} /BPC 1 /D [1 0] /F /AHx ID",
                xobj.width, xobj.height
            )
            .ok()?;
            xobj.sample_data
        }
        _ if !colored => return None,
        cs => {
            let name = match cs {
                ImageColorSpace::DeviceGray => "G",
                ImageColorSpace::DeviceRGB => "RGB",
                ImageColorSpace::DeviceCMYK => "CMYK",
                _ => return None,
            };
            if params.bits_per_component != 8 || params.jpeg.is_some() {
                return None;
            }
            let xobj = image_ops::convert_image(sample_data, params);
            if xobj.smask_data.is_some() || xobj.bits_per_component != 8 {
                return None;
            }
            writeln!(
                out,
                "BI /W {} /H {} /CS /{name} /BPC 8 /F /AHx ID",
                xobj.width, xobj.height
            )
            .ok()?;
            xobj.sample_data
        }
    };
    for (i, b) in data.iter().enumerate() {
        if i > 0 && i % 64 == 0 {
            out.push(b'\n');
        }
        write!(out, "{b:02X}").ok()?;
    }
    out.extend(b">\nEI\n");
    Some(out)
}

/// Write one font and its glyph procedures; return the font's object.
fn write_font(writer: &mut PdfWriter, font: &Type3Font) -> u32 {
    let first = font.glyphs.keys().next().copied().unwrap_or(0);
    let last = font.glyphs.keys().next_back().copied().unwrap_or(0);

    // One procedure name per code: the glyph name where it is a name no
    // other code of the font uses, else one made from the code.
    let mut taken: HashMap<Vec<u8>, u8> = HashMap::new();
    let mut names: BTreeMap<u8, Vec<u8>> = BTreeMap::new();
    for (&code, proc) in &font.glyphs {
        let name = proc
            .glyph_name
            .clone()
            .filter(|n| !n.is_empty() && n.as_slice() != b".notdef" && !taken.contains_key(n))
            .unwrap_or_else(|| format!("g{code}").into_bytes());
        taken.insert(name.clone(), code);
        names.insert(code, name);
    }

    let mut char_procs = Vec::new();
    let mut differences = Vec::new();
    let mut previous: Option<u8> = None;
    for (&code, proc) in &font.glyphs {
        let name = names[&code].clone();
        let r = writer.add_stream(Vec::new(), &proc.content, true);
        char_procs.push((name.clone(), PdfObj::Ref(r)));
        if previous.is_none_or(|p| u16::from(p) + 1 != u16::from(code)) {
            differences.push(PdfObj::Int(i64::from(code)));
        }
        differences.push(PdfObj::Name(name));
        previous = Some(code);
    }

    let widths: Vec<PdfObj> = (first..=last)
        .map(|code| PdfObj::Real(font.glyphs.get(&code).map_or(0.0, |g| g.width)))
        .collect();

    let mut bbox = Bounds::default();
    for proc in font.glyphs.values() {
        if let Some([x0, y0, x1, y1]) = proc.bbox {
            bbox.add((x0, y0), 0.0);
            bbox.add((x1, y1), 0.0);
        }
    }
    let bbox = bbox.rect().unwrap_or([0.0; 4]);
    let fm = font.font_matrix;

    let mut entries = vec![
        (b"Type".to_vec(), PdfObj::name("Font")),
        (b"Subtype".to_vec(), PdfObj::name("Type3")),
    ];
    if !font.font_name.is_empty() {
        entries.push((b"Name".to_vec(), PdfObj::Name(font.font_name.clone())));
    }
    entries.extend([
        (
            b"FontBBox".to_vec(),
            PdfObj::Array(bbox.iter().map(|&v| PdfObj::Real(v)).collect()),
        ),
        (
            b"FontMatrix".to_vec(),
            PdfObj::Array(
                [fm.a, fm.b, fm.c, fm.d, fm.tx, fm.ty]
                    .iter()
                    .map(|&v| PdfObj::Real(v))
                    .collect(),
            ),
        ),
        (b"CharProcs".to_vec(), PdfObj::Dict(char_procs)),
        (
            b"Encoding".to_vec(),
            PdfObj::Dict(vec![
                (b"Type".to_vec(), PdfObj::name("Encoding")),
                (b"Differences".to_vec(), PdfObj::Array(differences)),
            ]),
        ),
        (b"FirstChar".to_vec(), PdfObj::Int(i64::from(first))),
        (b"LastChar".to_vec(), PdfObj::Int(i64::from(last))),
        (b"Widths".to_vec(), PdfObj::Array(widths)),
        // The procedures name no resources: their images are inline.
        (b"Resources".to_vec(), PdfObj::Dict(Vec::new())),
    ]);

    let unicode = tounicode_map(font);
    if !unicode.is_empty() {
        let cmap_name = String::from_utf8_lossy(&font.font_name)
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .collect::<String>();
        let cmap_name = if cmap_name.is_empty() {
            font.pdf_name.clone()
        } else {
            cmap_name
        };
        let cmap = crate::pdf_device::generate_tounicode_cmap(&unicode, &cmap_name);
        let r = writer.add_stream(Vec::new(), &cmap, true);
        entries.push((b"ToUnicode".to_vec(), PdfObj::Ref(r)));
    }
    writer.add_object(&PdfObj::Dict(entries))
}

/// Each code's Unicode value, from its glyph name: the Adobe Glyph List
/// and the `uniXXXX` and `uXXXX` forms. Codes whose name says nothing are
/// left out.
fn tounicode_map(font: &Type3Font) -> HashMap<u16, u16> {
    font.glyphs
        .iter()
        .filter_map(|(&code, proc)| {
            let name = std::str::from_utf8(proc.glyph_name.as_deref()?).ok()?;
            let u = stet_fonts::agl::glyph_name_to_unicode(name)?;
            Some((u16::from(code), u))
        })
        .collect()
}

/// A running bounding box.
#[derive(Default)]
struct Bounds(Option<[f64; 4]>);

impl Bounds {
    fn add(&mut self, (x, y): (f64, f64), pad: f64) {
        let r = self.0.get_or_insert([x - pad, y - pad, x + pad, y + pad]);
        r[0] = r[0].min(x - pad);
        r[1] = r[1].min(y - pad);
        r[2] = r[2].max(x + pad);
        r[3] = r[3].max(y + pad);
    }

    /// Every point of `path`, the control points of its curves too (which
    /// bound the curve), `pad` around each.
    fn add_path(&mut self, path: &PsPath, pad: f64) {
        for seg in &path.segments {
            match *seg {
                PathSegment::MoveTo(x, y) | PathSegment::LineTo(x, y) => self.add((x, y), pad),
                PathSegment::CurveTo {
                    x1,
                    y1,
                    x2,
                    y2,
                    x3,
                    y3,
                } => {
                    self.add((x1, y1), pad);
                    self.add((x2, y2), pad);
                    self.add((x3, y3), pad);
                }
                PathSegment::ClosePath => {}
            }
        }
    }

    fn rect(&self) -> Option<[f64; 4]> {
        self.0
    }
}

fn emit_cm(buf: &mut Vec<u8>, m: &Matrix) {
    for v in [m.a, m.b, m.c, m.d, m.tx, m.ty] {
        fmt_num(buf, v);
        buf.push(b' ');
    }
    buf.extend(b"cm\n");
}

/// A device colour's components and its fill (`upper` false) or stroke
/// operator.
fn emit_color(buf: &mut Vec<u8>, c: &DeviceColor, upper: bool) {
    let op: &[u8] = if let Some((cc, m, y, k)) = c.native_cmyk {
        for v in [cc, m, y, k] {
            fmt_num(buf, v);
            buf.push(b' ');
        }
        if upper { b"K" } else { b"k" }
    } else if c.r == c.g && c.g == c.b {
        fmt_num(buf, c.r);
        buf.push(b' ');
        if upper { b"G" } else { b"g" }
    } else {
        for v in [c.r, c.g, c.b] {
            fmt_num(buf, v);
            buf.push(b' ');
        }
        if upper { b"RG" } else { b"rg" }
    };
    buf.extend(op);
    buf.push(b'\n');
}

fn emit_fill_color(buf: &mut Vec<u8>, c: &DeviceColor) {
    emit_color(buf, c, false);
}

fn emit_stroke_color(buf: &mut Vec<u8>, c: &DeviceColor) {
    emit_color(buf, c, true);
}

/// The pen: width, caps, joins, miter limit and dashes, all in device space.
fn emit_line_state(buf: &mut Vec<u8>, params: &StrokeParams) {
    fmt_num(buf, params.line_width);
    let cap = match params.line_cap {
        LineCap::Round => 1,
        LineCap::Square => 2,
        _ => 0,
    };
    let join = match params.line_join {
        LineJoin::Round => 1,
        LineJoin::Bevel => 2,
        _ => 0,
    };
    let _ = write!(buf, " w {cap} J {join} j ");
    fmt_num(buf, params.miter_limit);
    buf.extend(b" M [");
    for (i, &d) in params.dash_pattern.array.iter().enumerate() {
        if i > 0 {
            buf.push(b' ');
        }
        fmt_num(buf, d);
    }
    buf.extend(b"] ");
    fmt_num(buf, params.dash_pattern.offset);
    buf.extend(b" d\n");
}

#[cfg(test)]
mod tests {
    use super::*;
    use stet_graphics::device::FillParams;

    fn square() -> PsPath {
        let mut p = PsPath::new();
        p.segments = vec![
            PathSegment::MoveTo(0.0, 0.0),
            PathSegment::LineTo(500.0, 0.0),
            PathSegment::LineTo(500.0, 500.0),
            PathSegment::ClosePath,
        ];
        p
    }

    /// A glyph of a 1000-unit font at 10 units a glyph em, placed at
    /// `(x, y)` on a Y-down device.
    fn glyph_at(x: f64, y: f64, elements: usize) -> Type3Glyph {
        let fm = Matrix::new(0.01, 0.0, 0.0, 0.01, 0.0, 0.0);
        let ctm = Matrix::new(1.0, 0.0, 0.0, -1.0, 0.0, 800.0);
        let g = ctm.multiply(&Matrix::translate(x, y)).multiply(&fm);
        Type3Glyph::new(
            g,
            fm,
            (600.0, 0.0),
            Some([0.0, 0.0, 500.0, 500.0]),
            true,
            Some(b"A".to_vec()),
            elements,
        )
    }

    fn text(code: u8) -> TextParams {
        TextParams {
            text: vec![code],
            font_type: 3,
            font_name: b"Proc".to_vec(),
            ..TextParams::default()
        }
    }

    fn fill_for(glyph: &Type3Glyph) -> DisplayElement {
        DisplayElement::Fill {
            path: square().transform(&glyph.glyph_to_device),
            params: FillParams::default(),
        }
    }

    #[test]
    fn a_filled_glyph_goes_back_to_glyph_space() {
        let glyph = glyph_at(100.0, 200.0, 1);
        let proc = char_proc(&glyph, &[fill_for(&glyph)]).expect("kept");
        let content = String::from_utf8(proc.content).unwrap();
        assert_eq!(
            content,
            "600 0 0 0 500 500 d1\n0 0 m\n500 0 l\n500 500 l\nh\nf\n"
        );
    }

    #[test]
    fn two_showings_share_one_procedure() {
        let mut fonts = Type3Fonts::default();
        let a = glyph_at(100.0, 200.0, 1);
        let b = glyph_at(300.0, 50.0, 1);
        let first = fonts
            .place(&text(65), &a, &[fill_for(&a)])
            .map(|p| p.pdf_name.to_string());
        let second = fonts.place(&text(65), &b, &[fill_for(&b)]);
        let second = second.expect("kept");
        assert_eq!(first.as_deref(), Some("T0"));
        assert_eq!(second.pdf_name, "T0");
        // Text space is glyph space through the FontMatrix: the origin of
        // the second showing lands at its point on the device.
        let (x, y) = second.text_matrix.transform_point(0.0, 0.0);
        assert!(
            (x - 300.0).abs() < 1e-9 && (y - 750.0).abs() < 1e-9,
            "{x} {y}"
        );
        assert_eq!(fonts.fonts.len(), 1);
    }

    #[test]
    fn a_code_drawn_differently_starts_another_font() {
        let mut fonts = Type3Fonts::default();
        let a = glyph_at(100.0, 200.0, 1);
        fonts.place(&text(65), &a, &[fill_for(&a)]).expect("kept");
        let mut other = square();
        other.segments.pop();
        let b = glyph_at(10.0, 10.0, 1);
        let mark = DisplayElement::Fill {
            path: other.transform(&b.glyph_to_device),
            params: FillParams::default(),
        };
        let placed = fonts.place(&text(65), &b, &[mark]).expect("kept");
        assert_eq!(placed.pdf_name, "T1");
    }

    #[test]
    fn a_space_draws_nothing_and_still_is_text() {
        let mut fonts = Type3Fonts::default();
        let g = glyph_at(0.0, 0.0, 0);
        assert!(fonts.place(&text(32), &g, &[]).is_some());
    }

    #[test]
    fn marks_a_procedure_cannot_hold_stay_on_the_page() {
        let g = glyph_at(0.0, 0.0, 1);
        assert!(char_proc(&g, &[DisplayElement::InitClip]).is_none());
        let see_through = DisplayElement::Fill {
            path: square().transform(&g.glyph_to_device),
            params: FillParams {
                alpha: 0.5,
                ..FillParams::default()
            },
        };
        assert!(char_proc(&g, &[see_through]).is_none());
        // Nor does text in a spot colour, or text drawn as outlines.
        let mut fonts = Type3Fonts::default();
        let mut stroked = text(65);
        stroked.paint_type = 2;
        assert!(fonts.place(&stroked, &g, &[fill_for(&g)]).is_none());
    }

    #[test]
    fn a_glyph_with_its_own_colours_sets_them() {
        let mut g = glyph_at(0.0, 0.0, 1);
        g.stencil = false;
        g.bbox = None;
        let red = DisplayElement::Fill {
            path: square().transform(&g.glyph_to_device),
            params: FillParams {
                color: DeviceColor::from_rgb(1.0, 0.0, 0.0),
                ..FillParams::default()
            },
        };
        let proc = char_proc(&g, &[red]).expect("kept");
        let content = String::from_utf8(proc.content).unwrap();
        assert!(content.starts_with("600 0 d0\n1 0 0 rg\n"), "{content}");
        assert_eq!(proc.bbox, Some([0.0, 0.0, 500.0, 500.0]));
    }

    #[test]
    fn a_bitmap_glyph_is_an_inline_stencil() {
        let g = glyph_at(0.0, 0.0, 1);
        // An 8 × 2 mask filling the glyph's box, as `imagemask` records it.
        let params = ImageParams {
            width: 8,
            height: 2,
            color_space: ImageColorSpace::Mask {
                color: DeviceColor::default(),
                polarity: true,
                spot_color: None,
            },
            bits_per_component: 1,
            ctm: g.glyph_to_device,
            image_matrix: Matrix::new(8.0 / 500.0, 0.0, 0.0, -2.0 / 500.0, 0.0, 2.0),
            ..ImageParams::default()
        };
        let image = DisplayElement::Image {
            sample_data: std::sync::Arc::new(vec![0xF0, 0x0F]),
            params,
        };
        let proc = char_proc(&g, &[image]).expect("kept");
        let content = String::from_utf8(proc.content).unwrap();
        assert!(
            content.contains("BI /IM true /W 8 /H 2 /BPC 1 /D [1 0] /F /AHx ID\nF00F>\nEI\n"),
            "{content}"
        );
    }

    #[test]
    fn the_font_names_its_glyphs_and_their_text() {
        let mut fonts = Type3Fonts::default();
        let a = glyph_at(0.0, 0.0, 1);
        fonts.place(&text(65), &a, &[fill_for(&a)]).expect("kept");
        let mut writer = PdfWriter::new();
        let written = fonts.write(&mut writer);
        assert_eq!(written.len(), 1);
        let unicode = tounicode_map(&fonts.fonts[0]);
        assert_eq!(unicode.get(&65), Some(&0x41));
    }
}

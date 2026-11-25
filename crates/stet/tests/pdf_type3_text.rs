// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! PDF output keeps text in PostScript Type 3 fonts as text, in PDF Type 3
//! fonts whose glyph procedures draw what the PostScript ones drew.
//!
//! The PDF writer used to draw a Type 3 glyph as the marks its procedure
//! made, so the PDF had no text in such a font at all: nothing to search,
//! copy or carry on into PostScript as text. A glyph whose marks a PDF
//! glyph procedure cannot hold is still drawn as those marks.
#![cfg(feature = "pdf-output")]

use stet::{DisplayElement, Interpreter, PsDisplayList, TextExtraction};
use stet_fonts::geometry::PathSegment;

/// Each fill's and stencil image's bounding box, in order, at every depth.
fn boxes(list: &PsDisplayList, out: &mut Vec<[f64; 4]>) {
    for e in list.elements() {
        match e {
            DisplayElement::Fill { path, .. } => {
                let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
                for s in &path.segments {
                    let pts: &[(f64, f64)] = match s {
                        PathSegment::MoveTo(x, y) | PathSegment::LineTo(x, y) => &[(*x, *y)],
                        PathSegment::CurveTo { x3, y3, .. } => &[(*x3, *y3)],
                        PathSegment::ClosePath => &[],
                    };
                    for &(x, y) in pts {
                        b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
                    }
                }
                out.push(b);
            }
            DisplayElement::Image { params, .. } => {
                let m = params
                    .ctm
                    .multiply(&params.image_matrix.invert().unwrap())
                    .multiply(&stet_fonts::geometry::Matrix::scale(
                        params.width as f64,
                        params.height as f64,
                    ));
                let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
                for (u, v) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)] {
                    let (x, y) = m.transform_point(u, v);
                    b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
                }
                out.push(b);
            }
            DisplayElement::Group { elements, .. } | DisplayElement::OcgGroup { elements, .. } => {
                boxes(elements, out)
            }
            _ => {}
        }
    }
}

fn texts(list: &PsDisplayList, out: &mut Vec<String>) {
    for e in list.elements() {
        match e {
            DisplayElement::TextRun { params } => out.push(params.text.clone()),
            DisplayElement::Group { elements, .. } | DisplayElement::OcgGroup { elements, .. } => {
                texts(elements, out)
            }
            _ => {}
        }
    }
}

/// The job's PDF and page 1's text as a reader extracts it, the PDF checked
/// to draw every mark where the PostScript does.
fn pdf_and_text(job: &str) -> (String, Vec<String>) {
    let pages = Interpreter::new()
        .render_to_display_list(job.as_bytes(), 72.0)
        .unwrap();
    let mut ps = Vec::new();
    boxes(&pages[0].display_list, &mut ps);
    let pdf = Interpreter::new()
        .render_to_pdf(job.as_bytes(), 72.0)
        .unwrap();
    let mut doc = stet_pdf_reader::PdfDocument::from_bytes(&pdf).unwrap();
    doc.set_text_extraction(TextExtraction::Runs);
    let list = doc.render_page(0, 72.0).unwrap();
    let mut from_pdf = Vec::new();
    boxes(&list, &mut from_pdf);
    assert_eq!(
        from_pdf.len(),
        ps.len(),
        "marks: PDF {from_pdf:?}, PS {ps:?}"
    );
    for (pdf, ps) in from_pdf.iter().zip(&ps) {
        assert!(
            pdf.iter().zip(ps).all(|(a, b)| (a - b).abs() < 0.05),
            "PDF {pdf:?}, PS {ps:?}"
        );
    }
    let mut t = Vec::new();
    texts(&list, &mut t);
    // The PDF uncompressed enough to look for its fonts: the font
    // dictionaries are not in streams.
    (String::from_utf8_lossy(&pdf).into_owned(), t)
}

const PAGE: &str = "%!PS\n<< /PageSize [300 200] >> setpagedevice\n";

/// A Type 3 font whose glyphs are boxes of different widths, in a 1000-unit
/// glyph space, and whose space draws nothing.
const BOXES: &str = "/Boxes 9 dict dup begin\n\
  /FontType 3 def /FontMatrix [0.001 0 0 0.001 0 0] def /FontBBox [0 0 1000 1000] def\n\
  /Encoding 256 array 0 1 255 { 1 index exch /.notdef put } for\n\
    dup 32 /space put dup 65 /A put dup 66 /B put dup 67 /C put def\n\
  /W << /space 300 /A 600 /B 700 /C 800 /.notdef 0 >> def\n\
  /BuildChar { exch begin Encoding exch get dup W exch get exch\n\
    1 index 0 0 0 4 index 1000 setcachedevice\n\
    /space ne { 50 0 moveto dup 100 sub 0 rlineto 0 700 rlineto closepath fill } if\n\
    pop end } def\n\
end definefont pop\n";

#[test]
fn type3_text_is_text_in_a_pdf_type3_font() {
    let job = format!(
        "{PAGE}{BOXES}/Boxes findfont 20 scalefont setfont\n\
         20 150 moveto (ABC CAB) show\n\
         showpage\n"
    );
    let (pdf, text) = pdf_and_text(&job);
    assert!(pdf.contains("/Subtype /Type3"), "no Type 3 font");
    assert!(pdf.contains("/ToUnicode"), "no ToUnicode");
    assert_eq!(text, ["ABC CAB"]);
}

/// The same font at three sizes, one of them rotated, is one PDF font: its
/// glyph procedures are in glyph space. (Each size is a font of its own
/// here, as the interpreter keeps one set of built glyphs per font,
/// whatever the CTM.)
#[test]
fn sizes_and_rotations_share_one_font() {
    let job = format!(
        "{PAGE}{BOXES}/Boxes findfont 20 scalefont setfont 20 150 moveto (AB) show\n\
         /Boxes findfont 10 scalefont setfont 20 100 moveto (BA) show\n\
         gsave 150 50 translate 30 rotate /Boxes findfont 15 scalefont setfont\n\
           0 0 moveto (CAB) show grestore\n\
         showpage\n"
    );
    let (pdf, text) = pdf_and_text(&job);
    assert_eq!(pdf.matches("/Subtype /Type3").count(), 1, "one font");
    assert_eq!(text, ["AB", "BA", "CAB"]);
}

/// A bitmap font: each glyph an `imagemask`, through `BuildGlyph`.
#[test]
fn bitmap_glyphs_are_stencils_in_the_font() {
    let job = format!(
        "{PAGE}/Bits 9 dict dup begin\n\
           /FontType 3 def /FontMatrix [1 0 0 1 0 0] def /FontBBox [0 0 8 8] def\n\
           /Encoding 256 array 0 1 255 {{ 1 index exch /.notdef put }} for\n\
             dup 72 /H put dup 105 /i put def\n\
           /Bits << /H <C3C3C3FFFFC3C3C3> /i <1818001818181818> /.notdef <0000000000000000> >> def\n\
           /BuildGlyph {{ exch begin Bits exch get 9 0 0 0 8 8 setcachedevice\n\
             8 8 true [1 0 0 -1 0 8] 5 -1 roll imagemask end }} def\n\
         end definefont pop\n\
         /Bits findfont 2 scalefont setfont 20 150 moveto (Hi) show\n\
         showpage\n"
    );
    let (pdf, text) = pdf_and_text(&job);
    assert!(pdf.contains("/Subtype /Type3"), "no Type 3 font");
    assert_eq!(text, ["Hi"]);
}

/// A glyph that sets its own colour (`setcharwidth`) keeps it: the
/// procedure is a `d0` one with its colour in it.
#[test]
fn coloured_glyphs_keep_their_colour() {
    let job = format!(
        "{PAGE}/Paint 9 dict dup begin\n\
           /FontType 3 def /FontMatrix [0.001 0 0 0.001 0 0] def /FontBBox [0 0 1000 1000] def\n\
           /Encoding 256 array 0 1 255 {{ 1 index exch /.notdef put }} for dup 82 /R put def\n\
           /BuildChar {{ pop pop 700 0 setcharwidth 1 0 0 setrgbcolor\n\
             50 0 600 700 rectfill }} def\n\
         end definefont pop\n\
         /Paint findfont 20 scalefont setfont 20 150 moveto (RR) show\n\
         showpage\n"
    );
    let (pdf, text) = pdf_and_text(&job);
    assert!(pdf.contains("/Subtype /Type3"), "no Type 3 font");
    assert_eq!(text, ["RR"]);
}

/// A glyph painted with a pattern has nowhere to be in a PDF Type 3 font:
/// it is drawn as before, and there is no font.
#[test]
fn pattern_painted_glyphs_stay_marks() {
    let job = format!(
        "{PAGE}{BOXES}<< /PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 4 4]\n\
           /XStep 4 /YStep 4 /PaintProc {{ pop 0 0 2 2 rectfill }} >> matrix makepattern\n\
         /Pattern setcolorspace setcolor\n\
         /Boxes findfont 20 scalefont setfont 20 150 moveto (AB) show\n\
         showpage\n"
    );
    let pdf = Interpreter::new()
        .render_to_pdf(job.as_bytes(), 72.0)
        .unwrap();
    assert!(!String::from_utf8_lossy(&pdf).contains("/Subtype /Type3"));
}

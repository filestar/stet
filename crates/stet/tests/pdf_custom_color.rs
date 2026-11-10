// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A custom colour (TN 5044 `findcmykcustomcolor` / `setcustomcolor`, which
//! Illustrator's AGM procset paints every spot colour with) stays a spot
//! colour in PDF output: a Separation with a CMYK alternate, as Ghostscript's
//! pdfwrite and Distiller write it. Other output paints its CMYK tint.
#![cfg(feature = "pdf-output")]

use stet::Interpreter;

const JOB: &str = "%!PS\n<< /PageSize [200 200] >> setpagedevice\n\
    0.1 0.9 0.8 0 (PANTONE 185 C) findcmykcustomcolor 0.5 setcustomcolor\n\
    10 10 100 100 rectfill showpage\n";

#[test]
fn a_custom_colour_is_a_separation_in_a_pdf() {
    let pdf = Interpreter::new().render_to_pdf(JOB.as_bytes(), 72.0).unwrap();
    let text = String::from_utf8_lossy(&pdf);
    assert!(text.contains("/Separation"), "a Separation colour space");
    assert!(text.contains("PANTONE#20185#20C"), "named after the custom colour");
}

#[test]
fn elsewhere_a_custom_colour_paints_its_cmyk_tint() {
    let mut interp = Interpreter::new();
    // After a PDF job the interpreter paints CMYK again.
    interp.render_to_pdf(JOB.as_bytes(), 72.0).unwrap();
    interp
        .exec(
            b"0.1 0.9 0.8 0 (X) findcmykcustomcolor 0.5 setcustomcolor \
              currentcolorspace 0 get /DeviceCMYK ne { stop } if \
              currentcmykcolor 0 ne { stop } if 0.4 sub abs 0.001 gt { stop } if \
              0.45 sub abs 0.001 gt { stop } if 0.05 sub abs 0.001 gt { stop } if",
        )
        .unwrap();
}

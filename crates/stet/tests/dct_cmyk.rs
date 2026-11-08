// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `DCTDecode` hands on a CMYK JPEG's samples as stored, as Ghostscript does: the program's
//! `Decode` array says how to read them.

use stet::Interpreter;

/// An 8 × 8 CMYK JPEG with Adobe's APP14 marker, written by Pillow for the colour
/// C 200 M 40 Y 120 K 10, which it stores inverted: 55 215 135 245.
const JPEG: [&str; 8] = [
    "ffd8ffee000e41646f626500640000000000ffdb00430002010101010102010101020202020204030202020205040403",
    "04060506060605060606070908060709070606080b08090a0a0a0a0a06080b0c0b0a0c090a0a0affc000140800080008",
    "044311004d11005911004b1100ffc4001f0000010501010101010100000000000000000102030405060708090a0bffc4",
    "00b5100002010303020403050504040000017d01020300041105122131410613516107227114328191a1082342b1c115",
    "52d1f02433627282090a161718191a25262728292a3435363738393a434445464748494a535455565758595a63646566",
    "6768696a737475767778797a838485868788898a92939495969798999aa2a3a4a5a6a7a8a9aab2b3b4b5b6b7b8b9bac2",
    "c3c4c5c6c7c8c9cad2d3d4d5d6d7d8d9dae1e2e3e4e5e6e7e8e9eaf1f2f3f4f5f6f7f8f9faffda000e0443004d005900",
    "4b00003f00fcdbafd5cadcafdd4affd9",
];

/// Blue when every check (each leaves a boolean) is true, red otherwise.
fn all_true(setup: &str, checks: &[&str]) -> [u8; 3] {
    let body: Vec<String> = checks.iter().map(|c| format!("{c} and")).collect();
    let src = format!(
        "%!PS\n{setup}\ntrue {} {{ 0 0 1 }} {{ 1 0 0 }} ifelse setrgbcolor 0 0 100 100 rectfill showpage\n",
        body.join(" ")
    );
    let mut interp = Interpreter::builder().suppress_output().build();
    let pages = interp.render(src.as_bytes(), 18.0).expect("renders");
    let w = pages[0].width as usize;
    let at = ((pages[0].height as usize - 5) * w + 5) * 4;
    [
        pages[0].rgba[at],
        pages[0].rgba[at + 1],
        pages[0].rgba[at + 2],
    ]
}

#[test]
fn a_cmyk_jpeg_decodes_to_its_stored_samples() {
    let setup = format!(
        "/s <{}> /DCTDecode filter 256 string readstring pop def",
        JPEG.concat()
    );
    // Ghostscript 10 reads 55 215 135 245; a sample within 2 of it allows for the IDCT.
    let near = |i: usize, v: i32| format!("s {i} get {v} sub abs 2 le");
    let checks = [near(0, 55), near(1, 215), near(2, 135), near(3, 245)];
    let checks: Vec<&str> = checks.iter().map(String::as_str).collect();
    assert_eq!(all_true(&setup, &checks), [0, 0, 255]);
}

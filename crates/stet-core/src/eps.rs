// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! EPS file support: DOS binary header stripping and BoundingBox parsing.

/// DOS EPS binary header magic bytes.
const DOS_EPS_MAGIC: [u8; 4] = [0xC5, 0xD0, 0xD3, 0xC6];

/// Strip a DOS EPS binary header if present, returning the PostScript portion.
///
/// DOS EPS files start with a 30-byte header containing magic bytes `C5 D0 D3 C6`,
/// followed by a little-endian u32 offset and u32 length pointing to the embedded
/// PostScript section. If the magic is not found, the data is returned unchanged.
pub fn strip_dos_eps_header(data: &[u8]) -> &[u8] {
    if data.len() < 12 || data[..4] != DOS_EPS_MAGIC {
        return data;
    }

    let ps_offset = u32::from_le_bytes([data[4], data[5], data[6], data[7]]) as usize;
    let ps_length = u32::from_le_bytes([data[8], data[9], data[10], data[11]]) as usize;

    let start = ps_offset.min(data.len());
    let end = (ps_offset + ps_length).min(data.len());
    &data[start..end]
}

/// Detect EPS by content: checks if the first line contains `EPSF` (e.g. `%!PS-Adobe-2.0 EPSF-1.2`).
///
/// This catches EPS files with `.ps` extensions that GhostScript would still treat as EPS.
pub fn content_is_epsf(data: &[u8]) -> bool {
    // Check just the first line (up to first newline or 256 bytes)
    let end = data
        .iter()
        .position(|&b| b == b'\n' || b == b'\r')
        .unwrap_or(data.len().min(256));
    let first_line = &data[..end];
    first_line.windows(4).any(|w| w == b"EPSF")
}

/// How far into the file the header comments are looked for. Illustrator and
/// QuarkXPress write header comments well past 4 KB (long `%%DocumentFonts`
/// and `%%+` continuation lists).
const HEADER_MAX: usize = 64 * 1024;

/// How far from the end a `(atend)` box is looked for.
const TRAILER_MAX: usize = 4096;

/// Parse the `%%BoundingBox` and `%%HiResBoundingBox` DSC comments of EPS data.
///
/// Read as DSC 3.0 says: from the header, the comment lines after the first
/// line up to `%%EndComments` or the first line that is not a comment (within
/// the first 64 KB), and the first of each comment there counts. A box after
/// the header belongs to something else, such as an EPS embedded between
/// `%%BeginDocument` and `%%EndDocument`. `(atend)` defers to the last such
/// comment in the trailer (the last 4 KB).
///
/// The HiRes box is preferred when it is finite and has an area; otherwise the
/// integer box is returned (which may itself have no area: callers check).
///
/// Returns `Some((llx, lly, urx, ury))` or `None` if not found.
pub fn read_eps_bounding_box(data: &[u8]) -> Option<(f64, f64, f64, f64)> {
    let head = &data[..data.len().min(HEADER_MAX)];
    let mut bbox = Found::Missing;
    let mut hires = Found::Missing;
    for line in head.split(is_eol).skip(1).filter(|l| !l.is_empty()) {
        if line.starts_with(b"%%EndComments") || !line.starts_with(b"%") {
            break;
        }
        if let Some(rest) = line.strip_prefix(b"%%HiResBoundingBox:") {
            hires.first(rest);
        } else if let Some(rest) = line.strip_prefix(b"%%BoundingBox:") {
            bbox.first(rest);
        }
    }

    let trailer = &data[data.len().saturating_sub(TRAILER_MAX)..];
    let bbox = bbox.resolve(trailer, b"%%BoundingBox:");
    let hires = hires.resolve(trailer, b"%%HiResBoundingBox:");
    hires.filter(|b| has_area(*b)).or(bbox)
}

/// One box comment as the header gives it.
enum Found {
    Missing,
    AtEnd,
    Box(Option<(f64, f64, f64, f64)>),
}

impl Found {
    /// The header's first comment counts: a later one changes nothing.
    fn first(&mut self, rest: &[u8]) {
        if matches!(self, Found::Missing) {
            *self = if trim_ascii(rest) == b"(atend)" {
                Found::AtEnd
            } else {
                Found::Box(parse_four_numbers(rest))
            };
        }
    }

    /// The box, with `(atend)` taken from the last such comment in the trailer.
    fn resolve(self, trailer: &[u8], key: &[u8]) -> Option<(f64, f64, f64, f64)> {
        match self {
            Found::Missing => None,
            Found::Box(b) => b,
            // From the end: the last one counts.
            Found::AtEnd => trailer
                .rsplit(is_eol)
                .filter_map(|l| l.strip_prefix(key))
                .filter(|r| trim_ascii(r) != b"(atend)")
                .find_map(parse_four_numbers),
        }
    }
}

fn is_eol(b: &u8) -> bool {
    *b == b'\n' || *b == b'\r'
}

fn has_area(b: (f64, f64, f64, f64)) -> bool {
    b.2 > b.0 && b.3 > b.1
}

/// Parse four whitespace-separated numbers from a byte slice.
fn parse_four_numbers(data: &[u8]) -> Option<(f64, f64, f64, f64)> {
    let s = std::str::from_utf8(data).ok()?;
    let mut nums = s.split_whitespace().filter_map(|w| w.parse::<f64>().ok());
    let llx = nums.next()?;
    let lly = nums.next()?;
    let urx = nums.next()?;
    let ury = nums.next()?;
    [llx, lly, urx, ury]
        .iter()
        .all(|v| v.is_finite())
        .then_some((llx, lly, urx, ury))
}

/// Trim leading and trailing ASCII whitespace from a byte slice.
fn trim_ascii(data: &[u8]) -> &[u8] {
    let start = data
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(data.len());
    let end = data
        .iter()
        .rposition(|b| !b.is_ascii_whitespace())
        .map_or(start, |p| p + 1);
    &data[start..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_dos_header() {
        // Build a synthetic DOS EPS file
        let ps_content = b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 100 100\n";
        let ps_offset: u32 = 30; // header is 30 bytes
        let ps_length = ps_content.len() as u32;

        let mut data = Vec::new();
        data.extend_from_slice(&DOS_EPS_MAGIC);
        data.extend_from_slice(&ps_offset.to_le_bytes());
        data.extend_from_slice(&ps_length.to_le_bytes());
        // Pad to 30 bytes (TIFF preview offset, length, and checksum)
        data.resize(30, 0xFF);
        data.extend_from_slice(ps_content);

        let result = strip_dos_eps_header(&data);
        assert_eq!(result, ps_content);
    }

    #[test]
    fn test_strip_no_header() {
        let ps_content = b"%!PS-Adobe-3.0\n%%BoundingBox: 0 0 200 300\n";
        let result = strip_dos_eps_header(ps_content);
        assert_eq!(result, ps_content);
    }

    #[test]
    fn test_content_is_epsf() {
        assert!(content_is_epsf(
            b"%!PS-Adobe-2.0 EPSF-1.2\n%%BoundingBox: 0 0 100 100\n"
        ));
        assert!(content_is_epsf(
            b"%!PS-Adobe-3.0 EPSF-3.0\r%%BoundingBox: 0 0 100 100\r"
        ));
        assert!(!content_is_epsf(
            b"%!PS-Adobe-3.0\n%%BoundingBox: 0 0 100 100\n"
        ));
        assert!(!content_is_epsf(b"/Helvetica findfont\n"));
    }

    #[test]
    fn test_bbox_integer() {
        let data = b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 200 300\n";
        let bbox = read_eps_bounding_box(data);
        assert_eq!(bbox, Some((0.0, 0.0, 200.0, 300.0)));
    }

    #[test]
    fn test_bbox_hires_preferred() {
        let data = b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 200 300\n%%HiResBoundingBox: 0.5 1.5 199.75 299.25\n";
        let bbox = read_eps_bounding_box(data);
        assert_eq!(bbox, Some((0.5, 1.5, 199.75, 299.25)));
    }

    #[test]
    fn test_bbox_not_found() {
        let data = b"%!PS-Adobe-3.0\n/Helvetica findfont 12 scalefont setfont\n";
        let bbox = read_eps_bounding_box(data);
        assert_eq!(bbox, None);
    }

    #[test]
    fn test_bbox_negative_coords() {
        let data = b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: -50 -25 150 275\n";
        let bbox = read_eps_bounding_box(data);
        assert_eq!(bbox, Some((-50.0, -25.0, 150.0, 275.0)));
    }

    #[test]
    fn test_bbox_cr_line_endings() {
        // Old Mac-style CR-only line endings (used by some Adobe Illustrator files)
        let data = b"%!PS-Adobe-3.0 EPSF-3.0\r%%BoundingBox: 0 9 360 135\r%%HiResBoundingBox: 0 9.001 360 135\r";
        let bbox = read_eps_bounding_box(data);
        assert_eq!(bbox, Some((0.0, 9.001, 360.0, 135.0)));
    }

    #[test]
    fn test_bbox_atend() {
        // Build data larger than 4096 bytes with (atend) in header and bbox in trailer
        let mut data = Vec::new();
        data.extend_from_slice(b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: (atend)\n");
        // Pad to > 4096 bytes
        data.resize(5000, b' ');
        data.extend_from_slice(b"\n%%BoundingBox: 10 20 300 400\n%%EOF\n");

        let bbox = read_eps_bounding_box(&data);
        assert_eq!(bbox, Some((10.0, 20.0, 300.0, 400.0)));
    }

    #[test]
    fn test_bbox_first_in_header_counts() {
        let data = b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 300 200\n%%BoundingBox: 0 0 50 50\n%%EndComments\n";
        assert_eq!(read_eps_bounding_box(data), Some((0.0, 0.0, 300.0, 200.0)));
    }

    #[test]
    fn test_bbox_after_the_header_is_not_the_files() {
        // An embedded EPS's box, after %%EndComments.
        let data = b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 300 200\n%%EndComments\n\
                     %%BeginDocument: x.eps\n%%BoundingBox: 0 0 40 40\n%%EndDocument\n";
        assert_eq!(read_eps_bounding_box(data), Some((0.0, 0.0, 300.0, 200.0)));
        // The header also ends at the first line that is not a comment.
        let data = b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 300 200\n/x 1 def\n%%BoundingBox: 0 0 40 40\n";
        assert_eq!(read_eps_bounding_box(data), Some((0.0, 0.0, 300.0, 200.0)));
    }

    #[test]
    fn test_bbox_hires_without_area_is_not_preferred() {
        let data =
            b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 300 200\n%%HiResBoundingBox: 0 0 0 0\n";
        assert_eq!(read_eps_bounding_box(data), Some((0.0, 0.0, 300.0, 200.0)));
    }

    #[test]
    fn test_bbox_in_a_long_header() {
        // Illustrator's font lists run past 4 KB before the box.
        let mut data = b"%!PS-Adobe-3.0 EPSF-3.0\n%%DocumentFonts: A\n".to_vec();
        for _ in 0..600 {
            data.extend_from_slice(b"%%+ SomeFontName\n");
        }
        data.extend_from_slice(b"%%BoundingBox: 0 0 300 200\n%%EndComments\n");
        assert!(data.len() > 4096);
        assert_eq!(read_eps_bounding_box(&data), Some((0.0, 0.0, 300.0, 200.0)));
    }

    #[test]
    fn test_bbox_not_finite_is_ignored() {
        let data = b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 1e400 200\n";
        assert_eq!(read_eps_bounding_box(data), None);
    }
}

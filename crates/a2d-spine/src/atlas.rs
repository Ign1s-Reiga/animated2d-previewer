//! Atlas parser.
//!
//! Two dialects share one grammar and must both be accepted, because target
//! games ship both:
//!
//! * **Legacy** (libgdx, Spine 3.x and earlier) — `xy:`, `size:`, `orig:`,
//!   `offset:`, and `rotate: true|false`.
//! * **Modern** (libgdx 1.9.11+, Spine 4.x) — `bounds:`, `offsets:`, `pma:`,
//!   and `rotate:` as a degree count.
//!
//! Detection is per-key rather than per-file: exporters do mix them, and a file
//! that uses `bounds:` for one region and `xy:` for the next still parses.

use a2d_core::ir::atlas::{Atlas, AtlasPage, AtlasRegion, TextureFilter, TextureWrap};
use a2d_core::ir::ids::AtlasPageId;
use a2d_core::{DecodeError, LoadReport};

/// Parses atlas text into pages and regions.
///
/// Returns a report carrying every key the parser recognised but chose not to
/// act on, and every key it did not recognise at all.
pub fn parse_atlas(text: &str) -> Result<(Atlas, LoadReport), DecodeError> {
    let mut report = LoadReport::new();
    let mut atlas = Atlas::default();
    let mut lines = Lines::new(text);

    // `None` means the next non-blank line starts a page rather than a region.
    let mut page_open = false;

    while let Some((lineno, raw)) = lines.next_line() {
        let line = raw.trim();
        if line.is_empty() {
            page_open = false;
            continue;
        }

        if !page_open {
            let mut page = AtlasPage::new(line);
            while let Some((lineno, entry)) = lines.next_entry() {
                apply_page_entry(&mut page, &entry, lineno, &mut report)?;
            }
            atlas.pages.push(page);
            page_open = true;
            continue;
        }

        let page_id = AtlasPageId::from_index(atlas.pages.len() - 1)
            .ok_or_else(|| DecodeError::corrupt("atlas has more pages than the page handle can address"))?;
        let mut region = new_region(line, page_id);
        let mut saw_bounds = false;
        while let Some((lineno, entry)) = lines.next_entry() {
            apply_region_entry(&mut region, &entry, lineno, &mut saw_bounds, &mut report)?;
        }
        if !saw_bounds {
            return Err(DecodeError::corrupt_at(
                format!("atlas region {:?} has neither `bounds` nor `xy`/`size`", region.name),
                lineno as u64,
            ));
        }
        atlas.regions.push(region);
    }

    if atlas.pages.is_empty() {
        return Err(DecodeError::corrupt("atlas contains no pages"));
    }
    atlas.sort_regions();
    Ok((atlas, report))
}

fn new_region(name: &str, page: AtlasPageId) -> AtlasRegion {
    AtlasRegion {
        name: name.to_string(),
        page,
        xy: (0, 0),
        size: (0, 0),
        rotate_deg: 0,
        offset: (0, 0),
        original_size: (0, 0),
        index: -1,
        splits: None,
        pads: None,
    }
}

/// One `key: v1, v2, ...` line.
struct Entry {
    key: String,
    values: Vec<String>,
}

impl Entry {
    fn parse(line: &str) -> Option<Entry> {
        let line = line.trim();
        if line.is_empty() {
            return None;
        }
        let colon = line.find(':')?;
        Some(Entry {
            key: line[..colon].trim().to_string(),
            values: line[colon + 1..].split(',').map(|v| v.trim().to_string()).collect(),
        })
    }

    fn int(&self, i: usize, lineno: usize) -> Result<i32, DecodeError> {
        let raw = self.values.get(i).ok_or_else(|| {
            DecodeError::corrupt_at(format!("atlas key `{}` needs at least {} values", self.key, i + 1), lineno as u64)
        })?;
        raw.parse::<i32>().map_err(|_| {
            DecodeError::corrupt_at(format!("atlas key `{}` has non-integer value {raw:?}", self.key), lineno as u64)
        })
    }

    fn uint(&self, i: usize, lineno: usize) -> Result<u32, DecodeError> {
        let v = self.int(i, lineno)?;
        u32::try_from(v).map_err(|_| {
            DecodeError::corrupt_at(format!("atlas key `{}` has negative value {v}", self.key), lineno as u64)
        })
    }

    fn str(&self, i: usize) -> &str {
        self.values.get(i).map(String::as_str).unwrap_or("")
    }

    fn bool(&self, i: usize) -> bool {
        self.str(i).eq_ignore_ascii_case("true")
    }
}

/// A cursor that can look ahead one line, so a `key: value` run can be consumed
/// without swallowing the name line that follows it.
struct Lines<'a> {
    inner: std::iter::Enumerate<std::str::Lines<'a>>,
    peeked: Option<(usize, &'a str)>,
}

impl<'a> Lines<'a> {
    fn new(text: &'a str) -> Self {
        Lines { inner: text.lines().enumerate(), peeked: None }
    }

    fn next_line(&mut self) -> Option<(usize, &'a str)> {
        self.peeked.take().or_else(|| self.inner.next().map(|(i, l)| (i + 1, l)))
    }

    /// Consumes the next line only if it parses as a `key: value` entry.
    fn next_entry(&mut self) -> Option<(usize, Entry)> {
        let (lineno, line) = self.next_line()?;
        match Entry::parse(line) {
            Some(e) => Some((lineno, e)),
            None => {
                self.peeked = Some((lineno, line));
                None
            }
        }
    }
}

fn apply_page_entry(
    page: &mut AtlasPage,
    entry: &Entry,
    lineno: usize,
    report: &mut LoadReport,
) -> Result<(), DecodeError> {
    match entry.key.as_str() {
        "size" => page.size = Some((entry.uint(0, lineno)?, entry.uint(1, lineno)?)),
        "filter" => {
            page.min_filter = parse_filter(entry.str(0), report);
            // A single-value `filter:` applies to both.
            page.mag_filter = if entry.values.len() > 1 { parse_filter(entry.str(1), report) } else { page.min_filter };
        }
        "repeat" => {
            let (u, v) = match entry.str(0) {
                "none" | "" => (TextureWrap::ClampToEdge, TextureWrap::ClampToEdge),
                "x" => (TextureWrap::Repeat, TextureWrap::ClampToEdge),
                "y" => (TextureWrap::ClampToEdge, TextureWrap::Repeat),
                "xy" => (TextureWrap::Repeat, TextureWrap::Repeat),
                other => {
                    report.note(format!("atlas page {:?}: unknown repeat mode {other:?}, using none", page.name));
                    (TextureWrap::ClampToEdge, TextureWrap::ClampToEdge)
                }
            };
            page.u_wrap = u;
            page.v_wrap = v;
        }
        "pma" => page.premultiplied_alpha = entry.bool(0),
        // The GPU pixel format is chosen by the renderer from the decoded image,
        // so the exporter's preference is intentionally ignored.
        "format" => {}
        other => report.note(format!("atlas page {:?}: ignored unknown key `{other}`", page.name)),
    }
    Ok(())
}

fn apply_region_entry(
    region: &mut AtlasRegion,
    entry: &Entry,
    lineno: usize,
    saw_bounds: &mut bool,
    report: &mut LoadReport,
) -> Result<(), DecodeError> {
    match entry.key.as_str() {
        // Modern dialect.
        "bounds" => {
            region.xy = (entry.uint(0, lineno)?, entry.uint(1, lineno)?);
            region.size = (entry.uint(2, lineno)?, entry.uint(3, lineno)?);
            *saw_bounds = true;
            if region.original_size == (0, 0) {
                region.original_size = region.unrotated_size();
            }
        }
        "offsets" => {
            region.offset = (entry.int(0, lineno)?, entry.int(1, lineno)?);
            region.original_size = (entry.uint(2, lineno)?, entry.uint(3, lineno)?);
        }
        // Legacy dialect.
        "xy" => {
            region.xy = (entry.uint(0, lineno)?, entry.uint(1, lineno)?);
            *saw_bounds = true;
        }
        "size" => {
            region.size = (entry.uint(0, lineno)?, entry.uint(1, lineno)?);
            if region.original_size == (0, 0) {
                region.original_size = region.unrotated_size();
            }
        }
        "orig" => region.original_size = (entry.uint(0, lineno)?, entry.uint(1, lineno)?),
        "offset" => region.offset = (entry.int(0, lineno)?, entry.int(1, lineno)?),
        // Shared, but with two spellings of the value.
        "rotate" => {
            let raw = entry.str(0);
            region.rotate_deg = match raw {
                "true" => 90,
                "false" => 0,
                n => match n.parse::<u16>() {
                    Ok(d @ (0 | 90 | 180 | 270)) => d,
                    _ => {
                        report.note(format!(
                            "atlas region {:?}: unsupported rotate value {raw:?}, treating as 0",
                            region.name
                        ));
                        0
                    }
                },
            };
        }
        "index" => region.index = entry.int(0, lineno)?,
        "split" => {
            region.splits =
                Some([entry.int(0, lineno)?, entry.int(1, lineno)?, entry.int(2, lineno)?, entry.int(3, lineno)?])
        }
        "pad" => {
            region.pads =
                Some([entry.int(0, lineno)?, entry.int(1, lineno)?, entry.int(2, lineno)?, entry.int(3, lineno)?])
        }
        other => report.note(format!("atlas region {:?}: ignored unknown key `{other}`", region.name)),
    }
    Ok(())
}

fn parse_filter(s: &str, report: &mut LoadReport) -> TextureFilter {
    match s {
        "Nearest" => TextureFilter::Nearest,
        "Linear" => TextureFilter::Linear,
        "MipMap" => TextureFilter::MipMap,
        "MipMapNearestNearest" => TextureFilter::MipMapNearestNearest,
        "MipMapLinearNearest" => TextureFilter::MipMapLinearNearest,
        "MipMapNearestLinear" => TextureFilter::MipMapNearestLinear,
        "MipMapLinearLinear" => TextureFilter::MipMapLinearLinear,
        other => {
            report.note(format!("atlas: unknown filter {other:?}, using Linear"));
            TextureFilter::Linear
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEGACY: &str = "\n\
character.png\n\
size: 1024,2048\n\
format: RGBA8888\n\
filter: Linear,Linear\n\
repeat: none\n\
head\n\
  rotate: false\n\
  xy: 2, 2\n\
  size: 100, 120\n\
  orig: 110, 130\n\
  offset: 5, 6\n\
  index: -1\n\
arm\n\
  rotate: true\n\
  xy: 104, 2\n\
  size: 40, 90\n\
  orig: 90, 40\n\
  offset: 0, 0\n\
  index: -1\n";

    const MODERN: &str = "character.png\n\
size: 1024, 2048\n\
filter: Linear, Linear\n\
pma: true\n\
head\n\
index: -1\n\
bounds: 2, 2, 100, 120\n\
offsets: 5, 6, 110, 130\n\
arm\n\
bounds: 104, 2, 40, 90\n\
rotate: 90\n";

    #[test]
    fn legacy_dialect_parses_pages_and_regions() {
        let (atlas, report) = parse_atlas(LEGACY).unwrap();
        assert_eq!(atlas.pages.len(), 1);
        assert_eq!(atlas.pages[0].name, "character.png");
        assert_eq!(atlas.pages[0].size, Some((1024, 2048)));
        assert!(!atlas.pages[0].premultiplied_alpha);
        assert_eq!(atlas.regions.len(), 2);
        assert!(report.is_empty(), "{report}");
    }

    #[test]
    fn legacy_region_fields_land_in_the_right_places() {
        let (atlas, _) = parse_atlas(LEGACY).unwrap();
        let head = atlas.region(atlas.find("head").unwrap()).unwrap();
        assert_eq!(head.xy, (2, 2));
        assert_eq!(head.size, (100, 120));
        assert_eq!(head.original_size, (110, 130));
        assert_eq!(head.offset, (5, 6));
        assert_eq!(head.index, -1);
        assert_eq!(head.rotate_deg, 0);
    }

    #[test]
    fn legacy_rotate_true_means_ninety_degrees() {
        let (atlas, _) = parse_atlas(LEGACY).unwrap();
        let arm = atlas.region(atlas.find("arm").unwrap()).unwrap();
        assert_eq!(arm.rotate_deg, 90);
        // `size` is the art's own size, whichever way the packer laid it down.
        assert_eq!(arm.size, (40, 90));
        assert_eq!(arm.unrotated_size(), (40, 90));
        assert_eq!(arm.packed_size(), (90, 40), "on the sheet it lies on its side");
    }

    #[test]
    fn modern_dialect_parses_to_the_same_shape() {
        let (legacy, _) = parse_atlas(LEGACY).unwrap();
        let (modern, _) = parse_atlas(MODERN).unwrap();
        let l = legacy.region(legacy.find("head").unwrap()).unwrap();
        let m = modern.region(modern.find("head").unwrap()).unwrap();
        assert_eq!(l.xy, m.xy);
        assert_eq!(l.size, m.size);
        assert_eq!(l.original_size, m.original_size);
        assert_eq!(l.offset, m.offset);
    }

    #[test]
    fn modern_pma_flag_is_read() {
        let (atlas, _) = parse_atlas(MODERN).unwrap();
        assert!(atlas.pages[0].premultiplied_alpha);
    }

    #[test]
    fn modern_numeric_rotate_is_read_verbatim() {
        let (atlas, _) = parse_atlas(MODERN).unwrap();
        let arm = atlas.region(atlas.find("arm").unwrap()).unwrap();
        assert_eq!(arm.rotate_deg, 90);
    }

    #[test]
    fn bounds_without_offsets_defaults_original_size_to_the_unrotated_size() {
        let (atlas, _) = parse_atlas(MODERN).unwrap();
        let arm = atlas.region(atlas.find("arm").unwrap()).unwrap();
        // `bounds` came before `rotate`, so original size is the pre-rotation size.
        assert_eq!(arm.original_size, (40, 90));
    }

    #[test]
    fn multiple_pages_are_separated_by_blank_lines() {
        let text = "p0.png\nsize: 8,8\na\nbounds: 0,0,1,1\n\np1.png\nsize: 16,16\nb\nbounds: 0,0,2,2\n";
        let (atlas, _) = parse_atlas(text).unwrap();
        assert_eq!(atlas.pages.len(), 2);
        assert_eq!(atlas.region(atlas.find("a").unwrap()).unwrap().page, AtlasPageId(0));
        assert_eq!(atlas.region(atlas.find("b").unwrap()).unwrap().page, AtlasPageId(1));
    }

    #[test]
    fn a_page_with_no_properties_is_accepted() {
        let text = "p.png\nregion\nbounds: 0,0,4,4\n";
        let (atlas, _) = parse_atlas(text).unwrap();
        assert_eq!(atlas.pages.len(), 1);
        assert_eq!(atlas.regions.len(), 1);
        assert_eq!(atlas.pages[0].size, None);
    }

    #[test]
    fn single_value_filter_applies_to_both_directions() {
        let text = "p.png\nfilter: Nearest\nr\nbounds: 0,0,1,1\n";
        let (atlas, _) = parse_atlas(text).unwrap();
        assert_eq!(atlas.pages[0].min_filter, TextureFilter::Nearest);
        assert_eq!(atlas.pages[0].mag_filter, TextureFilter::Nearest);
    }

    #[test]
    fn repeat_modes_map_to_wrap_pairs() {
        for (repeat, u, v) in [
            ("none", TextureWrap::ClampToEdge, TextureWrap::ClampToEdge),
            ("x", TextureWrap::Repeat, TextureWrap::ClampToEdge),
            ("y", TextureWrap::ClampToEdge, TextureWrap::Repeat),
            ("xy", TextureWrap::Repeat, TextureWrap::Repeat),
        ] {
            let text = format!("p.png\nrepeat: {repeat}\nr\nbounds: 0,0,1,1\n");
            let (atlas, report) = parse_atlas(&text).unwrap();
            assert_eq!(atlas.pages[0].u_wrap, u, "repeat={repeat}");
            assert_eq!(atlas.pages[0].v_wrap, v, "repeat={repeat}");
            assert!(report.is_empty());
        }
    }

    #[test]
    fn nine_patch_split_and_pad_are_kept() {
        let text = "p.png\nr\nbounds: 0,0,10,10\nsplit: 1,2,3,4\npad: 5,6,7,8\n";
        let (atlas, _) = parse_atlas(text).unwrap();
        let r = atlas.region(atlas.find("r").unwrap()).unwrap();
        assert_eq!(r.splits, Some([1, 2, 3, 4]));
        assert_eq!(r.pads, Some([5, 6, 7, 8]));
    }

    #[test]
    fn unknown_keys_are_reported_rather_than_dropped_silently() {
        let text = "p.png\nfuturekey: 1\nr\nbounds: 0,0,1,1\notherkey: 2\n";
        let (_, report) = parse_atlas(text).unwrap();
        let text = report.to_string();
        assert!(text.contains("futurekey"), "{text}");
        assert!(text.contains("otherkey"), "{text}");
    }

    #[test]
    fn unknown_filter_falls_back_and_reports() {
        let text = "p.png\nfilter: Cubic, Cubic\nr\nbounds: 0,0,1,1\n";
        let (atlas, report) = parse_atlas(text).unwrap();
        assert_eq!(atlas.pages[0].min_filter, TextureFilter::Linear);
        assert!(report.to_string().contains("Cubic"));
    }

    #[test]
    fn unsupported_rotate_value_reports_and_falls_back() {
        let text = "p.png\nr\nbounds: 0,0,1,1\nrotate: 45\n";
        let (atlas, report) = parse_atlas(text).unwrap();
        assert_eq!(atlas.region(atlas.find("r").unwrap()).unwrap().rotate_deg, 0);
        assert!(report.to_string().contains("45"));
    }

    #[test]
    fn an_empty_atlas_is_an_error_not_an_empty_result() {
        assert!(parse_atlas("").is_err());
        assert!(parse_atlas("\n\n  \n").is_err());
    }

    #[test]
    fn a_region_with_no_position_is_corrupt() {
        let err = parse_atlas("p.png\nsize: 8,8\nr\nindex: 0\n").unwrap_err();
        assert!(matches!(err, DecodeError::Corrupt { .. }), "{err}");
        assert!(err.to_string().contains("neither"), "{err}");
    }

    #[test]
    fn non_integer_coordinates_are_corrupt_with_a_line_number() {
        let err = parse_atlas("p.png\nr\nbounds: 0,0,ten,1\n").unwrap_err();
        match err {
            DecodeError::Corrupt { at: Some(line), .. } => assert_eq!(line, 3),
            other => panic!("expected a located corruption, got {other}"),
        }
    }

    #[test]
    fn a_truncated_value_list_is_corrupt() {
        let err = parse_atlas("p.png\nr\nbounds: 0,0\n").unwrap_err();
        assert!(err.to_string().contains("at least"), "{err}");
    }

    #[test]
    fn negative_page_coordinates_are_rejected() {
        let err = parse_atlas("p.png\nr\nbounds: -1,0,1,1\n").unwrap_err();
        assert!(err.to_string().contains("negative"), "{err}");
    }

    #[test]
    fn regions_are_sorted_by_name_so_lookup_works() {
        let text = "p.png\nzebra\nbounds: 0,0,1,1\napple\nbounds: 1,1,1,1\n";
        let (atlas, _) = parse_atlas(text).unwrap();
        assert_eq!(atlas.regions[0].name, "apple");
        assert!(atlas.find("zebra").is_some());
    }

    #[test]
    fn windows_line_endings_parse() {
        let text = "p.png\r\nsize: 4,4\r\nr\r\nbounds: 0,0,1,1\r\n";
        let (atlas, report) = parse_atlas(text).unwrap();
        assert_eq!(atlas.pages[0].size, Some((4, 4)));
        assert_eq!(atlas.pages[0].name, "p.png");
        assert!(atlas.find("r").is_some());
        assert!(report.is_empty(), "{report}");
    }

    #[test]
    fn region_names_containing_slashes_are_preserved() {
        let text = "p.png\nbody/torso/front\nbounds: 0,0,1,1\n";
        let (atlas, _) = parse_atlas(text).unwrap();
        assert!(atlas.find("body/torso/front").is_some());
    }
}

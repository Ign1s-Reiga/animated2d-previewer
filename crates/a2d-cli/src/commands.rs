//! Command implementations.
//!
//! Every command prints the [`LoadReport`] it produced. Spec §16 requires
//! degradations to be surfaced, and a warning that no CLI surface prints is a
//! bug — so the printing lives here, once, and every path goes through it.

use std::path::Path;
use std::sync::Arc;

use a2d_core::{AnimatedModel, DecodeError, LoadReport, RuntimeError};
use a2d_import::games::{self, Importer};
use a2d_import::generic;
use a2d_pack::Package;
use a2d_render::{
    Camera, FrameSettings, GpuContext, OffscreenTarget, RenderError, Renderer, Rgba8Image, SamplerConfig, Viewport,
};
use a2d_runtime::GenericSpineModel;

/// Anything a command can fail with.
///
/// Kept local to the CLI rather than folded into [`DecodeError`]: a failed
/// `writeln!` to stdout is a property of this program, not of asset decoding,
/// and [`DecodeError::Io`] carries an asset path that would be a lie here.
#[derive(Debug)]
pub enum CliError {
    Decode(DecodeError),
    Runtime(RuntimeError),
    Render(RenderError),
    Output(std::io::Error),
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CliError::Decode(e) => write!(f, "{e}"),
            CliError::Runtime(e) => write!(f, "{e}"),
            CliError::Render(e) => write!(f, "{e}"),
            CliError::Output(e) => write!(f, "could not write output: {e}"),
        }
    }
}

impl std::error::Error for CliError {}

impl From<DecodeError> for CliError {
    fn from(e: DecodeError) -> Self {
        CliError::Decode(e)
    }
}

impl From<RuntimeError> for CliError {
    fn from(e: RuntimeError) -> Self {
        CliError::Runtime(e)
    }
}

impl From<RenderError> for CliError {
    fn from(e: RenderError) -> Self {
        CliError::Render(e)
    }
}

impl From<std::io::Error> for CliError {
    fn from(e: std::io::Error) -> Self {
        CliError::Output(e)
    }
}

/// Renders a report to a writer, or says the load was clean.
pub fn print_report(out: &mut dyn std::io::Write, report: &LoadReport) -> std::io::Result<()> {
    if report.is_empty() {
        writeln!(out, "\nLoaded cleanly.")
    } else {
        writeln!(out, "\n{report}")
    }
}

fn resolve_importer(input: &Path, requested: Option<&str>) -> Result<Importer, DecodeError> {
    match requested {
        None => Ok(games::guess_importer(input)),
        Some(name) => Importer::parse(name).ok_or_else(|| {
            DecodeError::UnsupportedFormat(format!(
                "unknown importer {name:?}; expected one of: {}",
                Importer::all().iter().map(|i| i.as_str()).collect::<Vec<_>>().join(", ")
            ))
        }),
    }
}

/// `animated2d inspect <input>`
pub fn inspect(out: &mut dyn std::io::Write, input: &Path, game: Option<&str>) -> Result<(), CliError> {
    let importer = resolve_importer(input, game)?;
    writeln!(out, "Input:    {}", input.display())?;
    writeln!(out, "Importer: {}", importer.as_str())?;

    // Identify every file first, so a directory that holds nothing loadable
    // still tells the user what it does hold.
    let listing = classify_path(input);
    if !listing.is_empty() {
        writeln!(out, "\nFiles:")?;
        for (name, kind) in &listing {
            writeln!(out, "  {name}  —  {kind}")?;
        }
    }

    let sets = games::discover(importer, input)?;
    let mut report = LoadReport::new();

    for set in &sets {
        writeln!(out, "\nCharacter: {}", set.name)?;
        writeln!(out, "  skeleton: {}", set.skeleton.display())?;
        writeln!(out, "  version:  spine {}", set.skeleton_version)?;
        match &set.atlas {
            Some(atlas) => writeln!(out, "  atlas:    {}", atlas.display())?,
            None => writeln!(out, "  atlas:    (none found)")?,
        }

        let ir = generic::decode_ir(set, &mut report)?;

        writeln!(out, "  bones:    {}", ir.bones.len())?;
        writeln!(out, "  slots:    {}", ir.slots.len())?;
        writeln!(out, "  skins:    {}", ir.skins.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join(", "))?;
        writeln!(out, "  attachments: {}", ir.attachment_count())?;

        writeln!(out, "  textures:")?;
        if ir.atlas.pages.is_empty() {
            writeln!(out, "    (none)")?;
        }
        for page in &ir.atlas.pages {
            let size = page.size.map(|(w, h)| format!("{w}x{h}")).unwrap_or_else(|| "size unknown".into());
            let pma = if page.premultiplied_alpha { ", premultiplied" } else { "" };
            writeln!(out, "    {} ({size}{pma})", page.name)?;
        }

        writeln!(out, "  animations:")?;
        if ir.animations.is_empty() {
            writeln!(out, "    (none)")?;
        }
        for animation in &ir.animations {
            writeln!(
                out,
                "    {:<28} {:>7.3}s  {} timelines",
                animation.name,
                animation.duration,
                animation.timelines.len()
            )?;
        }

        // Bounds come from the runtime posing the setup pose, which is the same
        // path the viewer takes.
        let model = GenericSpineModel::load(Arc::new(ir), &set.name);
        let bounds = model.bounds();
        if bounds.is_empty() {
            writeln!(out, "  bounds:   (empty — nothing drawable)")?;
        } else {
            let size = bounds.size();
            writeln!(
                out,
                "  bounds:   {:.1},{:.1} to {:.1},{:.1}  ({:.0}x{:.0})",
                bounds.min.x, bounds.min.y, bounds.max.x, bounds.max.y, size.x, size.y
            )?;
        }
        model.absorb_degradations(&mut report);
    }

    print_report(out, &report)?;
    Ok(())
}

/// `animated2d import <input> -o <output>`
pub fn import(out: &mut dyn std::io::Write, input: &Path, output: &Path, game: Option<&str>) -> Result<(), CliError> {
    let importer = resolve_importer(input, game)?;
    let sets = games::discover(importer, input)?;

    if sets.len() > 1 {
        // Writing several characters into one package directory would silently
        // clobber; ask for a narrower input instead.
        return Err(DecodeError::Ambiguous { candidates: sets.iter().map(|s| s.name.clone()).collect() }.into());
    }
    let set = &sets[0];

    let (package, report) = games::import(importer, set)?;
    package.write_to(output)?;

    writeln!(out, "Imported {} -> {}", set.name, output.display())?;
    writeln!(out, "  model type:    {}", package.manifest.model_type.as_str())?;
    writeln!(out, "  source format: {}", package.manifest.source_format)?;
    writeln!(out, "  source game:   {}", package.manifest.source_game)?;
    writeln!(out, "  animations:    {}", package.manifest.animations.len())?;
    writeln!(out, "  textures:      {}", package.textures.len())?;
    if let Some(default) = &package.manifest.default_animation {
        writeln!(out, "  default:       {default}")?;
    }
    print_report(out, &report)?;
    Ok(())
}

/// `animated2d validate <package>`
pub fn validate(out: &mut dyn std::io::Write, package_dir: &Path) -> Result<bool, CliError> {
    let package = Package::read_from(package_dir)?;
    writeln!(out, "Package:  {}", package_dir.display())?;
    writeln!(out, "  name:          {}", package.manifest.display_name)?;
    writeln!(out, "  model type:    {}", package.manifest.model_type.as_str())?;
    writeln!(out, "  source format: {}", package.manifest.source_format)?;
    writeln!(out, "  animations:    {}", package.manifest.animations.len())?;
    writeln!(out, "  textures:      {}/{}", package.textures.len(), package.manifest.textures.len())?;

    let report = package.validate();
    print_report(out, &report)?;
    Ok(report.is_empty())
}

/// Timestamps rendered by `preview` and by the visual regression tests.
///
/// Fixed by spec §17.3 so that a regression is compared against the same
/// instants every time.
pub const REGRESSION_TIMESTAMPS: [f32; 4] = [0.0, 0.25, 0.5, 1.0];

/// `animated2d preview <package> [-o <dir>]`
///
/// Renders the package at the regression timestamps. The desktop window is a
/// later phase; until it exists this is the real thing rendering real pixels,
/// which is more useful than a stub that opens nothing.
pub fn preview(out: &mut dyn std::io::Write, package_dir: &Path, frames_dir: Option<&Path>) -> Result<(), CliError> {
    let package = Package::read_from(package_dir)?;
    let ir = package
        .model
        .as_spine()
        .cloned()
        .ok_or_else(|| DecodeError::UnsupportedFormat("only Spine packages can be previewed".into()))?;
    let ir = Arc::new(ir);

    let mut model = GenericSpineModel::load(ir.clone(), &package.manifest.display_name);
    let animation =
        package.manifest.default_animation.clone().or_else(|| model.default_animation().map(str::to_string));

    writeln!(out, "Package:   {}", package_dir.display())?;
    writeln!(out, "Model:     {}", model.display_name())?;

    let gpu = GpuContext::headless()?;
    writeln!(out, "GPU:       {} ({})", gpu.adapter_name, gpu.backend)?;

    let mut renderer = Renderer::new(gpu.clone());
    let missing = upload_package_textures(&mut renderer, &gpu, &package, &ir)?;
    for name in &missing {
        writeln!(out, "  note: texture page {name:?} is missing; a placeholder was used")?;
    }

    let size = 512;
    let target = OffscreenTarget::new(&gpu, size, size)?;
    let viewport = Viewport::new(size, size);

    let Some(animation) = animation else {
        writeln!(out, "Animation: (none)")?;
        return Ok(());
    };
    writeln!(out, "Animation: {animation}")?;

    // One camera framing the setup pose, held across every frame — a camera
    // that refit per frame would hide exactly the movement being checked.
    model.pose_at(&animation, 0.0)?;
    let camera = Camera::fit(model.bounds(), viewport, 0.1);

    if let Some(dir) = frames_dir {
        std::fs::create_dir_all(dir).map_err(|e| DecodeError::io(dir.display().to_string(), e))?;
    }

    writeln!(
        out,
        "
Rendered frames:"
    )?;
    for time in REGRESSION_TIMESTAMPS {
        model.pose_at(&animation, time)?;
        let mut list = a2d_core::RenderList::new();
        model.emit(&mut list);

        let stats = renderer.render(target.view(), target.format(), FrameSettings::new(viewport, camera), &list)?;
        let image = target.read_pixels(&gpu)?;

        writeln!(
            out,
            "  t={time:<5}  {:>3} meshes  {:>3} draws  {:>5} tris  fingerprint {:016x}",
            list.meshes().len(),
            stats.draw_calls,
            stats.triangles,
            image.fingerprint()
        )?;

        if let Some(dir) = frames_dir {
            let path = dir.join(format!("frame_{time:.2}.png"));
            let png = a2d_render::encode_png(&image)?;
            std::fs::write(&path, png).map_err(|e| DecodeError::io(path.display().to_string(), e))?;
            writeln!(out, "           wrote {}", path.display())?;
        }
    }

    let mut report = LoadReport::new();
    model.absorb_degradations(&mut report);
    print_report(out, &report)?;
    Ok(())
}

/// Uploads a package's texture pages so their ids line up with the atlas.
///
/// The runtime emits `TextureId(page index)`, so upload order must follow
/// `atlas.pages` exactly. A page whose file is absent gets a placeholder rather
/// than being skipped — skipping would shift every later id and silently draw
/// the wrong art.
fn upload_package_textures(
    renderer: &mut Renderer,
    gpu: &GpuContext,
    package: &Package,
    ir: &a2d_core::ir::spine::SpineIr,
) -> Result<Vec<String>, CliError> {
    let mut missing = Vec::new();
    for page in &ir.atlas.pages {
        let sampler = SamplerConfig {
            min_filter: page.min_filter,
            mag_filter: page.mag_filter,
            u_wrap: page.u_wrap,
            v_wrap: page.v_wrap,
        };
        let file = package.textures.iter().find(|t| t.file == page.name);
        let decoded = file.map(|f| a2d_render::decode_png(&f.bytes, &page.name));

        match decoded {
            Some(Ok(image)) => {
                renderer.textures_mut().upload(gpu, &page.name, &image, page.premultiplied_alpha, sampler)?;
            }
            other => {
                if let Some(Err(e)) = other {
                    missing.push(format!("{} ({e})", page.name));
                } else {
                    missing.push(page.name.clone());
                }
                // Magenta placeholder, at this page's slot, so ids stay aligned.
                let placeholder = Rgba8Image::solid(2, 2, [255, 0, 255, 255]);
                renderer.textures_mut().upload(gpu, &page.name, &placeholder, false, SamplerConfig::default())?;
            }
        }
    }
    Ok(missing)
}

/// Classifies every file at `path` for the `inspect` listing.
fn classify_path(path: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if path.is_file() {
        if let Ok(bytes) = std::fs::read(path) {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("?").to_string();
            out.push((name, a2d_import::classify(&bytes).label()));
        }
        return out;
    }
    let Ok(entries) = std::fs::read_dir(path) else { return out };
    for entry in entries.flatten() {
        let file = entry.path();
        if !file.is_file() {
            continue;
        }
        let Some(name) = file.file_name().and_then(|n| n.to_str()) else { continue };
        let Ok(bytes) = std::fs::read(&file) else { continue };
        out.push((name.to_string(), a2d_import::classify(&bytes).label()));
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use a2d_core::Degradation;

    #[test]
    fn a_clean_report_says_so() {
        let mut out = Vec::new();
        print_report(&mut out, &LoadReport::new()).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "\nLoaded cleanly.\n");
    }

    #[test]
    fn warnings_are_printed_in_the_documented_shape() {
        let mut report = LoadReport::new();
        report.warn(Degradation::UnsupportedTimeline { animation: "idle".into(), kind: "path mix".into() });
        report.warn(Degradation::MissingReference { kind: "expression".into(), name: "smile_02".into() });
        let mut out = Vec::new();
        print_report(&mut out, &report).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("Loaded with warnings:"), "{text}");
        assert!(text.contains("- path mix timeline unsupported in animation \"idle\""), "{text}");
        assert!(text.contains("- missing expression: smile_02"), "{text}");
    }

    #[test]
    fn an_unknown_importer_name_lists_the_valid_ones() {
        let err = resolve_importer(Path::new("."), Some("genshin")).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("genshin"), "{text}");
        for name in ["generic", "aeons_echo", "depose_girls", "nikke"] {
            assert!(text.contains(name), "{text}");
        }
    }

    #[test]
    fn a_known_importer_name_resolves() {
        assert_eq!(resolve_importer(Path::new("."), Some("aeons_echo")).unwrap(), Importer::AeonsEcho);
    }

    #[test]
    fn classifying_a_missing_path_yields_nothing_rather_than_failing() {
        assert!(classify_path(Path::new("no/such/file")).is_empty());
    }
}

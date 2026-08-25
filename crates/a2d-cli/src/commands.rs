//! Command implementations.
//!
//! Every command prints the [`LoadReport`] it produced. Spec §16 requires
//! degradations to be surfaced, and a warning that no CLI surface prints is a
//! bug — so the printing lives here, once, and every path goes through it.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use a2d_core::{AnimatedModel, DecodeError, LoadReport, RuntimeError};
use a2d_desktop::{LoadedModel, Viewer, ViewerError};
use a2d_import::games::{self, Importer};
use a2d_import::generic;
use a2d_pack::Package;
use a2d_render::{GpuContext, OffscreenTarget, RenderError, Viewport};
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
    Viewer(ViewerError),
    Output(std::io::Error),
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CliError::Decode(e) => write!(f, "{e}"),
            CliError::Runtime(e) => write!(f, "{e}"),
            CliError::Render(e) => write!(f, "{e}"),
            CliError::Viewer(e) => write!(f, "{e}"),
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

impl From<ViewerError> for CliError {
    fn from(e: ViewerError) -> Self {
        CliError::Viewer(e)
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
    writeln!(out, "Input:    {}", input.display())?;

    // A Unity bundle holds a whole model rather than files on disk, so it is
    // opened directly. Which importer applies cannot be told from the container
    // alone -- Spine and Cubism ship in identical archives -- so the answer
    // comes from what is inside, and is printed there rather than guessed here.
    if let Some(bytes) = unity_bundle_bytes(input) {
        return inspect_unity_bundle(out, &bytes);
    }

    let importer = resolve_importer(input, game)?;
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

/// Timestamps rendered when exporting frames, and by the visual regression
/// tests.
///
/// Fixed by spec §17.3 so a regression is always compared at the same instants.
pub const REGRESSION_TIMESTAMPS: [f32; 4] = [0.0, 0.25, 0.5, 1.0];

/// Size of an exported frame, in pixels.
const EXPORT_SIZE: u32 = 512;

/// `animated2d preview <package> [-o <dir>]`
///
/// Without `-o`, opens the package in the desktop viewer (spec §14). With `-o`,
/// renders the regression timestamps offscreen and writes them as PNGs, which
/// is what a machine with no display or no compositor can still do.
pub fn preview(
    out: &mut dyn std::io::Write,
    package_dir: &Path,
    frames_dir: Option<&Path>,
    exit_after: Option<Duration>,
) -> Result<(), CliError> {
    match frames_dir {
        Some(dir) => export_frames(out, package_dir, dir),
        None => open_viewer(out, package_dir, exit_after),
    }
}

fn open_viewer(out: &mut dyn std::io::Write, package_dir: &Path, exit_after: Option<Duration>) -> Result<(), CliError> {
    writeln!(out, "Opening {} in the desktop viewer.", package_dir.display())?;
    writeln!(out, "  drag to move, scroll to scale, Space pauses, Tab cycles animations, Esc quits.")?;
    writeln!(out, "  everything is also in the tray menu.")?;
    if let Some(after) = exit_after {
        writeln!(out, "  quitting automatically after {:.1}s.", after.as_secs_f32())?;
    }
    writeln!(out)?;

    let mut report = LoadReport::new();
    let options = a2d_desktop::RunOptions { packages: vec![package_dir.to_path_buf()], exit_after };
    let result = a2d_desktop::run(options, &mut report);
    print_report(out, &report)?;

    let summary = result?;
    // Frames presented is the one observable proof that the window really drew,
    // and it is what a smoke test asserts on.
    writeln!(
        out,
        "
Presented {} frames in {:.1}s ({:.0} fps).",
        summary.frames,
        summary.elapsed.as_secs_f32(),
        summary.frames as f32 / summary.elapsed.as_secs_f32().max(f32::MIN_POSITIVE)
    )?;
    Ok(())
}

fn export_frames(out: &mut dyn std::io::Write, package_dir: &Path, frames_dir: &Path) -> Result<(), CliError> {
    let mut report = LoadReport::new();
    let model = LoadedModel::load(package_dir, &mut report)?;

    writeln!(out, "Package:   {}", package_dir.display())?;
    writeln!(out, "Model:     {}", model.model.display_name())?;
    let animation = model
        .model
        .default_animation()
        .map(str::to_string)
        .ok_or_else(|| DecodeError::UnsupportedFormat("the package has no animations to render".into()))?;
    let missing = model.missing_pages().len();
    if missing > 0 {
        writeln!(out, "  note: {missing} texture page(s) missing; placeholders were used")?;
    }

    let gpu = GpuContext::headless()?;
    writeln!(out, "GPU:       {} ({})", gpu.adapter_name, gpu.backend)?;
    let target = OffscreenTarget::new(&gpu, EXPORT_SIZE, EXPORT_SIZE)?;
    let viewport = Viewport::new(EXPORT_SIZE, EXPORT_SIZE);
    let mut viewer = Viewer::new(gpu.clone(), vec![model])?;

    writeln!(out, "Animation: {animation}")?;

    // One camera fitted from the setup pose and then held. Refitting per frame
    // would cancel out the very movement the frames exist to show.
    viewer.pose_at(&animation, 0.0)?;
    let camera = viewer.camera(viewport, 1.0, a2d_core::Vec2::ZERO);

    std::fs::create_dir_all(frames_dir).map_err(|e| DecodeError::io(frames_dir.display().to_string(), e))?;

    writeln!(
        out,
        "
Rendered frames:"
    )?;
    for time in REGRESSION_TIMESTAMPS {
        viewer.pose_at(&animation, time)?;
        let stats = viewer.render(target.view(), target.format(), viewport, camera, false)?;
        let image = target.read_pixels(&gpu)?;

        let path = frames_dir.join(format!("frame_{time:.2}.png"));
        std::fs::write(&path, a2d_render::encode_png(&image)?)
            .map_err(|e| DecodeError::io(path.display().to_string(), e))?;

        writeln!(
            out,
            "  t={time:<5}  {:>3} draws  {:>5} tris  fingerprint {:016x}  ->  {}",
            stats.draw_calls,
            stats.triangles,
            image.fingerprint(),
            path.display()
        )?;
    }

    viewer.active().model.absorb_degradations(&mut report);
    print_report(out, &report)?;
    Ok(())
}

/// Classifies every file at `path` for the `inspect` listing.
/// Prints what the MOC3 itself declares, beside what Unity recorded.
fn print_moc3(
    out: &mut dyn std::io::Write,
    model: &a2d_cubism::Moc3,
    inventory: &a2d_import::CubismInventory,
) -> Result<(), CliError> {
    let c = model.canvas;
    writeln!(out, "  canvas:      {} x {} px at {} px/unit", c.size.0, c.size.1, c.pixels_per_unit)?;

    // A count that disagrees with the Unity components means one of the two
    // readings is wrong, and saying which is which beats picking one.
    let rows: [(&str, u32, usize); 3] = [
        ("parameters", model.counts.parameters, inventory.parameters),
        ("parts", model.counts.parts, inventory.parts),
        ("drawables", model.counts.drawables, inventory.drawables),
    ];
    for (label, from_moc, from_unity) in rows {
        let note =
            if from_moc as usize == from_unity { String::new() } else { format!("  (Unity reports {from_unity})") };
        writeln!(out, "  {label:<12} {from_moc}{note}")?;
    }
    let vertices: usize = model.drawables.iter().map(|d| d.vertex_count()).sum();
    let triangles: usize = model.drawables.iter().map(|d| d.triangle_count()).sum();
    writeln!(out, "  mesh:        {vertices} vertices, {triangles} triangles")?;
    writeln!(
        out,
        "  keyforms:    {} ({} warp, {} drawable; not yet evaluated, so no pose)",
        model.keyforms.len(),
        model.keyforms.warp_offsets.len(),
        model.keyforms.drawable_offsets.len()
    )?;
    writeln!(
        out,
        "  deformers:   {} ({} warp, {} rotation)",
        model.counts.deformers, model.counts.warp_deformers, model.counts.rotation_deformers
    )?;
    if model.counts.glues > 0 {
        writeln!(out, "  glues:       {}", model.counts.glues)?;
    }
    if !model.is_verified_version() {
        writeln!(out, "  note:        MOC3 version {} is newer than any checked against a real model", model.version)?;
    }
    Ok(())
}

/// Reads `input` when it is a Unity bundle, by content rather than by name.
fn unity_bundle_bytes(input: &Path) -> Option<Vec<u8>> {
    if !input.is_file() {
        return None;
    }
    let bytes = std::fs::read(input).ok()?;
    matches!(a2d_import::classify(&bytes), a2d_import::AssetKind::UnityBundle { .. }).then_some(bytes)
}

/// Prints the structured inventory of a Unity bundle (spec §12).
fn inspect_unity_bundle(out: &mut dyn std::io::Write, bytes: &[u8]) -> Result<(), CliError> {
    let mut report = LoadReport::new();
    let inventory = a2d_import::inspect_bundle(bytes, &mut report)?;

    writeln!(out, "Importer: {}", if inventory.is_cubism() { "unity_cubism" } else { "(undetermined)" })?;
    writeln!(out, "\nUnity bundle")?;
    writeln!(out, "  built with:  {}", inventory.unity_revision)?;
    writeln!(out, "  objects:     {}", inventory.object_count)?;

    match &inventory.moc {
        Some(moc) => {
            writeln!(out, "\nCubism model: {}", moc.name)?;
            writeln!(out, "  moc3:        {} bytes, format version {}", moc.bytes.len(), moc.version)?;
            if let Some(path) = &moc.asset_path {
                writeln!(out, "  authored at: {path}")?;
            }
            // The Unity side counts components; the MOC3 counts what the
            // model itself declares. Reporting both is what makes a
            // disagreement visible rather than quietly averaged over.
            match a2d_cubism::Moc3::parse(&moc.bytes) {
                Ok(model) => print_moc3(out, &model, &inventory)?,
                Err(e) => {
                    writeln!(out, "  contents:    unreadable — {e}")?;
                    writeln!(out, "  parameters:  {} (counted from the Unity components)", inventory.parameters)?;
                    writeln!(out, "  parts:       {}", inventory.parts)?;
                    writeln!(out, "  drawables:   {}", inventory.drawables)?;
                }
            }
            writeln!(out, "  hierarchy:   {} GameObjects", inventory.game_objects)?;
        }
        None => writeln!(out, "\nNo CubismMoc found: this bundle holds no Live2D model.")?,
    }

    writeln!(out, "\n  textures:")?;
    if inventory.textures.is_empty() {
        writeln!(out, "    (none)")?;
    }
    for texture in &inventory.textures {
        match &texture.asset_path {
            Some(path) => writeln!(out, "    {}  ({path})", texture.name)?,
            None => writeln!(out, "    {}", texture.name)?,
        }
    }

    writeln!(out, "\n  motions:")?;
    if inventory.motions.is_empty() {
        writeln!(out, "    (none)")?;
    }
    for motion in &inventory.motions {
        // Fade data is what preserves a motion's original identity, so whether
        // it survived is worth saying per motion rather than as a total.
        let fade = if motion.has_fade_data { "" } else { "  (no fade data)" };
        writeln!(out, "    {}{fade}", motion.name)?;
    }

    if !inventory.animator_controllers.is_empty() {
        writeln!(out, "\n  animator controllers:")?;
        for name in &inventory.animator_controllers {
            writeln!(out, "    {name}")?;
        }
    }

    if !inventory.fade_sources.is_empty() {
        writeln!(out, "\n  original motion sources:")?;
        for path in &inventory.fade_sources {
            writeln!(out, "    {path}")?;
        }
    }

    print_report(out, &report)?;
    writeln!(
        out,
        "\nReconstruction is not implemented yet: this reports what the bundle holds, \nnot a package that can be loaded."
    )?;
    Ok(())
}

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
        let err = resolve_importer(Path::new("."), Some("not_a_known_shape")).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("not_a_known_shape"), "{text}");
        for name in ["generic", "spine_bytes", "unity_cubism", "unity_spine"] {
            assert!(text.contains(name), "{text}");
        }
    }

    #[test]
    fn a_known_importer_name_resolves() {
        assert_eq!(resolve_importer(Path::new("."), Some("spine_bytes")).unwrap(), Importer::SpineBytes);
    }

    #[test]
    fn classifying_a_missing_path_yields_nothing_rather_than_failing() {
        assert!(classify_path(Path::new("no/such/file")).is_empty());
    }
}

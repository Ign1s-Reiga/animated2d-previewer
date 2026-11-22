//! Command implementations.
//!
//! Every command prints the [`LoadReport`] it produced. Spec §16 requires
//! degradations to be surfaced, and a warning that no CLI surface prints is a
//! bug — so the printing lives here, once, and every path goes through it.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use a2d_core::{AnimatedModel, DecodeError, LoadReport, RenderList, Rgba, RuntimeError};
use a2d_cubism::{CubismEmit, CubismEval};
use a2d_desktop::{LoadedModel, Viewer, ViewerError};
use a2d_import::games::{self, Importer};
use a2d_import::generic;
use a2d_pack::Package;
use a2d_render::{Camera, FrameSettings, GpuContext, OffscreenTarget, RenderError, Renderer, Rgba8Image, Viewport};
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

/// One line on a model's physics rig, shared by `inspect` and `validate`.
fn physics_summary(physics: Option<&a2d_core::ir::cubism::Physics>) -> String {
    match physics {
        None => "none".to_string(),
        Some(p) => {
            let sum = |f: fn(&a2d_core::ir::cubism::PhysicsGroup) -> usize| p.groups.iter().map(f).sum::<usize>();
            format!(
                "{} groups, {} inputs, {} outputs, {} particles",
                p.groups.len(),
                sum(|g| g.inputs.len()),
                sum(|g| g.outputs.len()),
                sum(|g| g.particles.len())
            )
        }
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

    // A Cubism model arrives as one bundle holding one character, so it needs
    // none of the pairing that discovery does for loose Spine files.
    if importer == Importer::UnityCubism {
        let bytes = unity_bundle_bytes(input)
            .ok_or_else(|| DecodeError::UnsupportedFormat(format!("{} is not a Unity bundle", input.display())))?;
        let mut report = LoadReport::new();
        let package = a2d_import::import_bundle(&bytes, &mut report)?;
        package.write_to(output)?;
        return describe_import(out, &package.manifest.display_name.clone(), output, &package, &report);
    }

    let sets = games::discover(importer, input)?;

    if sets.len() > 1 {
        // Writing several characters into one package directory would silently
        // clobber; ask for a narrower input instead.
        return Err(DecodeError::Ambiguous { candidates: sets.iter().map(|s| s.name.clone()).collect() }.into());
    }
    let set = &sets[0];

    let (package, report) = games::import(importer, set)?;
    package.write_to(output)?;
    describe_import(out, &set.name, output, &package, &report)
}

/// The shape every `import` prints, whichever importer produced the package.
fn describe_import(
    out: &mut dyn std::io::Write,
    name: &str,
    output: &Path,
    package: &Package,
    report: &LoadReport,
) -> Result<(), CliError> {
    writeln!(out, "Imported {name} -> {}", output.display())?;
    writeln!(out, "  model type:    {}", package.manifest.model_type.as_str())?;
    writeln!(out, "  source format: {}", package.manifest.source_format)?;
    writeln!(out, "  source game:   {}", package.manifest.source_game)?;
    writeln!(out, "  animations:    {}", package.manifest.animations.len())?;
    writeln!(out, "  textures:      {}", package.textures.len())?;
    if let Some(default) = &package.manifest.default_animation {
        writeln!(out, "  default:       {default}")?;
    }
    print_report(out, report)?;
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
    if let Some(ir) = package.model.as_cubism() {
        writeln!(out, "  physics:       {}", physics_summary(ir.physics.as_ref()))?;
    }

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
/// Renders a Cubism model straight out of a Unity bundle.
///
/// The package pipeline does not reach Cubism yet, so this goes from bundle to
/// pixels directly. It exists to answer one question -- does the model come out
/// looking like the character -- which nothing structural can answer.
/// Opens a Spine rig from a Unity bundle in the desktop window.
///
/// Phase 5's goal: a Unity-packaged rig shown through the ordinary Generic
/// Spine runtime, with no Unity knowledge below the importer.
fn open_spine_bundle(
    out: &mut dyn std::io::Write,
    path: &Path,
    inventory: a2d_import::SpineInventory,
    mut report: LoadReport,
    frames_dir: Option<&Path>,
    flags: ViewerFlags,
) -> Result<(), CliError> {
    let (Some(skeleton), Some(atlas)) = (&inventory.skeleton, &inventory.atlas) else {
        return Err(DecodeError::UnsupportedFormat("this bundle holds no complete Spine rig".into()).into());
    };

    let text = String::from_utf8_lossy(&atlas.bytes);
    let (pages_ir, atlas_report) = a2d_spine::parse_atlas(&text)?;
    report.absorb(atlas_report);
    let (ir, detection) = a2d_spine::decode_skeleton(&skeleton.bytes, pages_ir, &mut report)?;
    writeln!(out, "Rig:       {} bones, {} slots, {}", ir.bones.len(), ir.slots.len(), detection.version)?;

    // Atlas pages name image files; the bundle holds them as Texture2D under
    // the same stem. A page with no match keeps its slot so texture ids stay
    // aligned with the atlas.
    let bundle = a2d_unity::Bundle::parse(&std::fs::read(path)?)?;
    let node = bundle
        .nodes
        .iter()
        .find(|n| n.is_serialized())
        .ok_or_else(|| DecodeError::UnsupportedFormat("the bundle holds no serialized file".into()))?;
    let file = a2d_unity::SerializedFile::parse(bundle.node_data(node)?)?;
    let decoded = a2d_unity::read_textures(&file)?;

    let stem = |name: &str| {
        name.rsplit('/').next().unwrap_or(name).rsplit_once('.').map(|(a, _)| a).unwrap_or(name).to_ascii_lowercase()
    };
    let mut pages = Vec::with_capacity(ir.atlas.pages.len());
    for page in &ir.atlas.pages {
        let sampler = a2d_render::SamplerConfig {
            min_filter: page.min_filter,
            mag_filter: page.mag_filter,
            u_wrap: page.u_wrap,
            v_wrap: page.v_wrap,
        };
        let wanted = stem(&page.name);
        let image = decoded.iter().find(|t| stem(&t.name) == wanted).map(|t| Rgba8Image {
            width: t.width,
            height: t.height,
            pixels: t.rgba.clone(),
        });
        if image.is_none() {
            report.note(format!("texture page {:?} is not in this bundle", page.name));
        } else {
            writeln!(out, "Texture:   {}", page.name)?;
        }
        pages.push((page.name.clone(), image, page.premultiplied_alpha, sampler));
    }

    let name = skeleton.name.rsplit_once('.').map(|(a, _)| a).unwrap_or(&skeleton.name).to_string();
    let mut model = a2d_runtime::GenericSpineModel::load(std::sync::Arc::new(ir), &name);
    // Pose before framing: a skeleton's setup pose can be degenerate, and the
    // bounds are what say whether there is anything to look at.
    if let Some(first) = model.default_animation().map(str::to_string) {
        let _ = a2d_core::AnimatedModel::pose_at(&mut model, &first, 0.0);
        writeln!(out, "Animation: {first}")?;
    }
    let bounds = a2d_core::AnimatedModel::bounds(&model);
    writeln!(out, "Bounds:    x {:.1}..{:.1}  y {:.1}..{:.1}", bounds.min.x, bounds.max.x, bounds.min.y, bounds.max.y)?;
    let loaded = a2d_desktop::LoadedModel::from_parts(path, Box::new(model), pages, &mut report);
    if let Some(dir) = frames_dir {
        return render_frames(out, loaded, dir, report);
    }

    writeln!(
        out,
        "
Opening {} in the desktop viewer.",
        path.display()
    )?;
    writeln!(out, "  drag to move, scroll to scale, Space pauses, Tab cycles animations, Esc quits.")?;
    if let Some(after) = flags.exit_after {
        writeln!(out, "  quitting automatically after {:.1}s.", after.as_secs_f32())?;
    }
    writeln!(out)?;

    let options = a2d_desktop::RunOptions { packages: Vec::new(), exit_after: flags.exit_after, hidden: flags.hidden };
    let result = a2d_desktop::run_with(options, vec![loaded], &mut report);
    print_report(out, &report)?;

    let summary = result?;
    writeln!(
        out,
        "
Presented {} frames in {:.1}s ({:.0} fps) on {} with {} alpha.",
        summary.frames,
        summary.elapsed.as_secs_f32(),
        summary.frames as f32 / summary.elapsed.as_secs_f32().max(f32::MIN_POSITIVE),
        summary.backend,
        summary.alpha_mode
    )?;
    writeln!(out, "{}", summary.pacing_line())?;
    Ok(())
}

/// One decoded texture page: its file name, the image (`None` when it could
/// not be had) and whether it is premultiplied.
type CubismPage = (String, Option<Rgba8Image>, bool);

/// Decodes the pages a Cubism package lists, one entry per manifest entry.
///
/// A page that is missing or does not decode keeps its slot as `None` and is
/// noted in `report`: skipping it would shift every later page into its place
/// and draw the wrong art.
fn cubism_pages(package: &Package, report: &mut LoadReport) -> Vec<CubismPage> {
    let mut pages = Vec::with_capacity(package.manifest.textures.len());
    for entry in &package.manifest.textures {
        let image = match package.textures.iter().find(|t| t.file == entry.file) {
            Some(file) => match a2d_render::decode_png(&file.bytes, &entry.file) {
                Ok(image) => Some(image),
                Err(e) => {
                    report.note(format!("texture page {:?} could not be decoded: {e}", entry.file));
                    None
                }
            },
            None => {
                report.note(format!("texture page {:?} is listed but missing from the package", entry.file));
                None
            }
        };
        pages.push((entry.file.clone(), image, entry.premultiplied_alpha));
    }
    pages
}

/// Whether a package opened from `input` was read back from disk, as opposed
/// to imported in memory from a Unity bundle right now. Decided from the input
/// itself (its signature, not its extension), so every preview path agrees.
fn is_read_back(input: &Path) -> bool {
    use std::io::Read;
    let mut head = [0u8; 16];
    let read = std::fs::File::open(input).ok().and_then(|mut f| f.read(&mut head).ok()).unwrap_or(0);
    !matches!(a2d_import::classify(&head[..read]), a2d_import::AssetKind::UnityBundle { .. })
}

/// Seeds a report with what the importer recorded in a package read from disk.
///
/// A package imported in memory (a bundle opened directly) already has its
/// warnings in the live import report, and `record_warnings` copied the same
/// ones into the manifest; seeding from the manifest as well would print each
/// twice. So the manifest is read back only when the package came from disk.
fn seed_report(recorded: &[String], report: &mut LoadReport, from_disk: bool) {
    if !from_disk {
        return;
    }
    for warning in recorded {
        report.note(format!("recorded at import: {warning}"));
    }
}

/// Turns a Cubism package into a viewer-ready model with its texture page.
///
/// Every Cubism path -- a package, or a bundle imported into one -- goes through
/// here, so the window and the frame export show the same thing, motions
/// included. `a2d-desktop` cannot build this itself (it may not depend on a
/// decoder, spec §3), so the model is built here and handed over behind the
/// shared interface.
fn cubism_loaded(
    out: &mut dyn std::io::Write,
    path: &Path,
    package: &Package,
    report: &mut LoadReport,
) -> Result<LoadedModel, CliError> {
    let ir = package
        .model
        .as_cubism()
        .ok_or_else(|| DecodeError::UnsupportedFormat("the package does not hold a Cubism model".into()))?;
    writeln!(out, "Model:     {} drawables, {} parameters", ir.drawables.len(), ir.parameters.len())?;

    // Every drawable samples page zero; a model needing more would need the per
    // drawable texture index, which is not wired through yet.
    let decoded = cubism_pages(package, report);
    let page = match decoded.first() {
        Some((name, Some(image), premultiplied)) => {
            writeln!(out, "Texture:   {name} {}x{}", image.width, image.height)?;
            (name.clone(), Some(image.clone()), *premultiplied, Default::default())
        }
        Some((name, None, premultiplied)) => {
            report.note(format!("texture page {name:?} is unavailable; a placeholder is drawn instead"));
            (name.clone(), None, *premultiplied, Default::default())
        }
        None => {
            report.note("the package lists no texture; a placeholder is drawn instead");
            ("missing".to_string(), None, false, Default::default())
        }
    };
    let extra = decoded.len().saturating_sub(1);
    if extra > 0 {
        report.note(format!("{extra} further texture page(s) are not drawn; only page zero is sampled"));
    }

    let model = a2d_cubism::GenericCubismModel::load(ir.clone(), package.manifest.display_name.clone());
    if !model.unstable().is_empty() {
        report.note(format!("{} drawable(s) could not be posed and are drawn undeformed", model.unstable().len()));
    }
    match a2d_core::preferred_animation(model.animations()) {
        Some(name) => writeln!(out, "Animation: {name} ({} motion(s))", model.animations().len())?,
        None => writeln!(out, "Animation: none; the model has no motions")?,
    }
    Ok(LoadedModel::from_parts(path, Box::new(model), vec![page], report))
}

/// Opens a Cubism package in the desktop window, or renders its frames.
///
/// `report` carries whatever produced the package (a bundle import, say).
/// `path` is the input the user gave; whether it was a bundle or a package on
/// disk decides if the recorded import warnings are the only copy.
fn preview_cubism(
    out: &mut dyn std::io::Write,
    path: &Path,
    package: &Package,
    mut report: LoadReport,
    frames_dir: Option<&Path>,
    flags: ViewerFlags,
) -> Result<(), CliError> {
    seed_report(&package.manifest.import_warnings, &mut report, is_read_back(path));
    if let Some(dir) = frames_dir {
        return render_cubism_frames(out, path, package, dir, report);
    }
    let loaded = cubism_loaded(out, path, package, &mut report)?;

    writeln!(
        out,
        "
Opening {} in the desktop viewer.",
        path.display()
    )?;
    writeln!(out, "  drag to move, scroll to scale, Space pauses, Tab cycles animations, Esc quits.")?;
    if let Some(after) = flags.exit_after {
        writeln!(out, "  quitting automatically after {:.1}s.", after.as_secs_f32())?;
    }
    writeln!(out)?;

    // No packages: the remembered list would otherwise open a Spine model too.
    let options = a2d_desktop::RunOptions { packages: Vec::new(), exit_after: flags.exit_after, hidden: flags.hidden };
    let result = a2d_desktop::run_with(options, vec![loaded], &mut report);
    print_report(out, &report)?;

    let summary = result?;
    writeln!(
        out,
        "
Presented {} frames in {:.1}s ({:.0} fps) on {} with {} alpha.",
        summary.frames,
        summary.elapsed.as_secs_f32(),
        summary.frames as f32 / summary.elapsed.as_secs_f32().max(f32::MIN_POSITIVE),
        summary.backend,
        summary.alpha_mode
    )?;
    writeln!(out, "{}", summary.pacing_line())?;
    Ok(())
}

/// Renders a Cubism package at the regression timestamps of its default motion.
///
/// A model with no motions has nothing to play, so it keeps a single rest-pose
/// frame rather than four identical ones.
fn render_cubism_frames(
    out: &mut dyn std::io::Write,
    path: &Path,
    package: &Package,
    frames_dir: &Path,
    mut report: LoadReport,
) -> Result<(), CliError> {
    let ir = package
        .model
        .as_cubism()
        .ok_or_else(|| DecodeError::UnsupportedFormat("the package does not hold a Cubism model".into()))?;
    if ir.motions.is_empty() {
        writeln!(out, "  note: the model has no motions; rendering its rest pose only (pose.png)")?;
        let pages = cubism_pages(package, &mut report);
        return render_cubism_pose(out, ir, &pages, frames_dir, &report);
    }
    let loaded = cubism_loaded(out, path, package, &mut report)?;
    // The camera is fitted from the model's canvas (see `GenericCubismModel::bounds`),
    // so the frames are framed the way the single rest-pose export is.
    render_frames(out, loaded, frames_dir, report)
}

/// Renders a Cubism model's rest pose to a single frame, once its pages exist.
fn render_cubism_pose(
    out: &mut dyn std::io::Write,
    model: &a2d_cubism::CubismIr,
    pages: &[CubismPage],
    frames_dir: &Path,
    report: &LoadReport,
) -> Result<(), CliError> {
    let pose = model.pose(&[]);
    writeln!(out, "Model:     {} drawables, {} parameters", model.drawables.len(), model.parameters.len())?;
    if !pose.is_stable() {
        writeln!(out, "  note: {} drawable(s) could not be posed", pose.unstable.len())?;
    }

    let mut list = RenderList::new();
    model.emit(&pose, a2d_core::TextureId(0), &mut list);
    writeln!(out, "Meshes:    {}", list.meshes().len())?;

    let gpu = GpuContext::headless()?;
    writeln!(out, "GPU:       {} ({})", gpu.adapter_name, gpu.backend)?;
    let mut renderer = Renderer::new(gpu.clone());
    match pages.first() {
        Some((name, Some(image), premultiplied)) => {
            writeln!(out, "Texture:   {name} {}x{}", image.width, image.height)?;
            let id = renderer.textures_mut().upload(&gpu, name, image, *premultiplied, Default::default())?;
            debug_assert_eq!(id, a2d_core::TextureId(0), "the first upload takes slot zero");
        }
        _ => {
            writeln!(out, "Texture:   none; drawing untextured")?;
            renderer.textures_mut().install_fallback(&gpu)?;
        }
    }

    let target = OffscreenTarget::new(&gpu, EXPORT_SIZE, EXPORT_SIZE)?;
    let viewport = Viewport::new(EXPORT_SIZE, EXPORT_SIZE);

    // Frame by the canvas, not by the posed bounds. A single drawable that
    // poses wrongly would otherwise dominate the fit and shrink the character
    // to nothing, which is exactly what happened first time round.
    let canvas = model.canvas;
    let (half_w, half_h) = (canvas.size.0 / canvas.pixels_per_unit * 0.5, canvas.size.1 / canvas.pixels_per_unit * 0.5);
    let mut frame = a2d_core::Aabb::EMPTY;
    frame.extend(a2d_core::Vec2::new(-half_w, -half_h));
    frame.extend(a2d_core::Vec2::new(half_w, half_h));
    if let Some((lo, hi)) = model.bounds(&pose) {
        writeln!(out, "Bounds:    x {:.3}..{:.3}  y {:.3}..{:.3}", lo.x, hi.x, lo.y, hi.y)?;
    }
    writeln!(out, "Canvas:    {:.3} x {:.3} units", half_w * 2.0, half_h * 2.0)?;
    let camera = Camera::fit(frame, viewport, 0.04);

    let settings = FrameSettings::new(viewport, camera).with_clear_color(Rgba::new(0.0, 0.0, 0.0, 0.0));
    let stats = renderer.render(target.view(), target.format(), settings, &list)?;
    let image = target.read_pixels(&gpu)?;

    std::fs::create_dir_all(frames_dir)
        .map_err(|e| DecodeError::corrupt(format!("could not create {}: {e}", frames_dir.display())))?;
    let path = frames_dir.join("pose.png");
    let png = a2d_render::encode_png(&image)?;
    std::fs::write(&path, png).map_err(|e| DecodeError::corrupt(format!("could not write {}: {e}", path.display())))?;

    let covered = image.pixels.chunks_exact(4).filter(|p| p[3] > 0).count();
    writeln!(out, "Wrote:     {}", path.display())?;
    writeln!(out, "Draws:     {} calls, {} triangles", stats.draw_calls, stats.triangles)?;
    writeln!(out, "Coverage:  {:.1}% of the frame", covered as f64 * 100.0 / (EXPORT_SIZE * EXPORT_SIZE) as f64)?;
    print_report(out, report)?;
    Ok(())
}

/// How `preview` opens the interactive viewer. Ignored when rendering frames.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ViewerFlags {
    /// Quit automatically after this long.
    pub exit_after: Option<Duration>,
    /// Create the window invisible and unfocused, for scripted runs and tests.
    pub hidden: bool,
}

pub fn preview(
    out: &mut dyn std::io::Write,
    package_dir: &Path,
    frames_dir: Option<&Path>,
    flags: ViewerFlags,
) -> Result<(), CliError> {
    // A Unity bundle is not a package, but it is what a Cubism model arrives
    // in, so `preview` opens one directly rather than refusing.
    if let Some(bytes) = unity_bundle_bytes(package_dir) {
        writeln!(out, "Input:     {}", package_dir.display())?;

        // A bundle holds one family or the other, and Spine is the decisive
        // check: a skeleton and an atlas, both recognised by content.
        let mut report = LoadReport::new();
        let spine = a2d_import::inspect_spine_bundle(&bytes, &mut report)?;
        if spine.is_spine() {
            return open_spine_bundle(out, package_dir, spine, report, frames_dir, flags);
        }

        // Through the importer, so the motions recovered from the bundle come
        // along; the model itself is built from the package, as for any other.
        let mut report = LoadReport::new();
        let package = a2d_import::import_bundle(&bytes, &mut report)?;
        return preview_cubism(out, package_dir, &package, report, frames_dir, flags);
    }

    if let Ok(package) = Package::read_from(package_dir) {
        if package.manifest.model_type == a2d_pack::ModelType::Cubism {
            writeln!(out, "Package:   {}", package_dir.display())?;
            return preview_cubism(out, package_dir, &package, LoadReport::new(), frames_dir, flags);
        }
    }

    match frames_dir {
        Some(dir) => export_frames(out, package_dir, dir),
        None => open_viewer(out, package_dir, flags),
    }
}

fn open_viewer(out: &mut dyn std::io::Write, package_dir: &Path, flags: ViewerFlags) -> Result<(), CliError> {
    writeln!(out, "Opening {} in the desktop viewer.", package_dir.display())?;
    writeln!(out, "  drag to move, scroll to scale, Space pauses, Tab cycles animations, Esc quits.")?;
    writeln!(out, "  everything is also in the tray menu.")?;
    if let Some(after) = flags.exit_after {
        writeln!(out, "  quitting automatically after {:.1}s.", after.as_secs_f32())?;
    }
    writeln!(out)?;

    let mut report = LoadReport::new();
    let options = a2d_desktop::RunOptions {
        packages: vec![package_dir.to_path_buf()],
        exit_after: flags.exit_after,
        hidden: flags.hidden,
    };
    let result = a2d_desktop::run(options, &mut report);
    print_report(out, &report)?;

    let summary = result?;
    // Frames presented is the one observable proof that the window really drew,
    // and it is what a smoke test asserts on.
    writeln!(
        out,
        "
Presented {} frames in {:.1}s ({:.0} fps) on {} with {} alpha.",
        summary.frames,
        summary.elapsed.as_secs_f32(),
        summary.frames as f32 / summary.elapsed.as_secs_f32().max(f32::MIN_POSITIVE),
        summary.backend,
        summary.alpha_mode
    )?;
    writeln!(out, "{}", summary.pacing_line())?;
    Ok(())
}

fn export_frames(out: &mut dyn std::io::Write, package_dir: &Path, frames_dir: &Path) -> Result<(), CliError> {
    let mut report = LoadReport::new();
    let model = LoadedModel::load(package_dir, &mut report)?;
    writeln!(out, "Package:   {}", package_dir.display())?;
    render_frames(out, model, frames_dir, report)
}

/// Renders the regression timestamps of a model that is already loaded.
fn render_frames(
    out: &mut dyn std::io::Write,
    model: LoadedModel,
    frames_dir: &Path,
    mut report: LoadReport,
) -> Result<(), CliError> {
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
        ("parameters:", model.counts.parameters, inventory.parameters),
        ("parts:", model.counts.parts, inventory.parts),
        ("drawables:", model.counts.drawables, inventory.drawables),
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
        "  keyforms:    {} ({} warp, {} drawable)",
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
        let pairs: usize = model.glues.iter().map(|g| g.pairs.len()).sum();
        writeln!(out, "  glues:       {} ({pairs} stitched vertex pairs, applied when posing)", model.counts.glues)?;
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
/// Prints what a Unity bundle holding a Spine rig contains.
///
/// Spine survives Unity intact -- the skeleton and atlas are the editor's own
/// bytes -- so this reports the same things `inspect` reports for loose files,
/// and says so.
fn inspect_spine_bundle(
    out: &mut dyn std::io::Write,
    inventory: a2d_import::SpineInventory,
    mut report: LoadReport,
) -> Result<(), CliError> {
    writeln!(out, "Importer: unity_spine")?;
    writeln!(
        out,
        "
Unity bundle"
    )?;
    writeln!(out, "  built with:  {}", inventory.unity_revision)?;
    writeln!(out, "  objects:     {}", inventory.object_count)?;
    if !inventory.components.is_empty() {
        writeln!(out, "  components:  {}", inventory.components.join(", "))?;
    }

    if let Some(skeleton) = &inventory.skeleton {
        writeln!(
            out,
            "
Spine rig: {}",
            skeleton.name
        )?;
        writeln!(out, "  skeleton:    {} bytes", skeleton.bytes.len())?;
        if let Some(kind) = &inventory.skeleton_kind {
            writeln!(out, "  version:     {}", kind.label())?;
        }
        if let Some(path) = &skeleton.asset_path {
            writeln!(out, "  authored at: {path}")?;
        }
    }
    if let Some(atlas) = &inventory.atlas {
        writeln!(out, "  atlas:       {} ({} bytes)", atlas.name, atlas.bytes.len())?;
    }

    // The rig itself, decoded exactly as a loose export would be. Doing it here
    // rather than only reporting sizes is what shows the extraction is faithful.
    if let (Some(skeleton), Some(atlas)) = (&inventory.skeleton, &inventory.atlas) {
        let text = String::from_utf8_lossy(&atlas.bytes);
        let decoded = a2d_spine::parse_atlas(&text).map_err(CliError::from).and_then(|(pages, atlas_report)| {
            report.absorb(atlas_report);
            Ok(a2d_spine::decode_skeleton(&skeleton.bytes, pages, &mut report)?)
        });
        match decoded {
            Ok((ir, _)) => {
                writeln!(out, "  bones:       {}", ir.bones.len())?;
                writeln!(out, "  slots:       {}", ir.slots.len())?;
                writeln!(out, "  attachments: {}", ir.attachments.len())?;
                writeln!(
                    out,
                    "
  animations:"
                )?;
                for animation in &ir.animations {
                    writeln!(out, "    {:<30} {:.3}s", animation.name, animation.duration)?;
                }
            }
            Err(e) => writeln!(out, "  contents:    unreadable — {e}")?,
        }
    }

    writeln!(
        out,
        "
  textures:"
    )?;
    if inventory.textures.is_empty() {
        writeln!(out, "    (none)")?;
    }
    for texture in &inventory.textures {
        match &texture.asset_path {
            Some(path) => writeln!(out, "    {}  ({path})", texture.name)?,
            None => writeln!(out, "    {}", texture.name)?,
        }
    }
    if !inventory.other_text_assets.is_empty() {
        writeln!(
            out,
            "
  other text assets: {}",
            inventory.other_text_assets.join(", ")
        )?;
    }

    print_report(out, &report)?;
    Ok(())
}

fn inspect_unity_bundle(out: &mut dyn std::io::Write, bytes: &[u8]) -> Result<(), CliError> {
    let mut report = LoadReport::new();

    // A bundle holds one family or the other. The Spine check runs first
    // because it is decisive -- a skeleton and an atlas, both recognised by
    // content -- whereas the Cubism path reports a structure either way.
    let spine = a2d_import::inspect_spine_bundle(bytes, &mut report)?;
    if spine.is_spine() {
        return inspect_spine_bundle(out, spine, report);
    }

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
        // Whether fade data survives decides how much of a motion's original
        // authoring is available; inspect does not decode, so it only says what
        // is present.
        let fade = if motion.has_fade_data { "  (fade data present)" } else { "  (clip only, no fade data)" };
        writeln!(out, "    {}{fade}", motion.name)?;
    }

    writeln!(out, "\n  physics:     {}", physics_summary(inventory.physics.as_ref()))?;

    print_controllers_and_sources(out, &inventory)?;

    print_report(out, &report)?;
    writeln!(
        out,
        "\nThis reports what the bundle holds. `animated2d import` reconstructs a loadable \npackage from it, and `animated2d preview` opens it directly."
    )?;
    Ok(())
}

/// The animator controllers and the original motion sources. Both always
/// print: an empty list is a finding (a bundle with no fade motion data is the
/// normal case for real exports), and omitting the heading would leave the
/// reader unable to tell "absent" from "not looked for".
fn print_controllers_and_sources(
    out: &mut dyn std::io::Write,
    inventory: &a2d_import::CubismInventory,
) -> std::io::Result<()> {
    writeln!(
        out,
        "
  animator controllers:"
    )?;
    if inventory.animator_controllers.is_empty() {
        writeln!(out, "    (none: no AnimatorController in this bundle)")?;
    }
    for name in &inventory.animator_controllers {
        writeln!(out, "    {name}")?;
    }

    writeln!(
        out,
        "
  original motion sources:"
    )?;
    if inventory.fade_sources.is_empty() {
        writeln!(out, "    (none: no fade motion entry names a .motion3.json)")?;
    }
    for path in &inventory.fade_sources {
        writeln!(out, "    {path}")?;
    }
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
    fn the_physics_summary_counts_or_says_none() {
        use a2d_core::ir::cubism::{Physics, PhysicsGroup, PhysicsInput, PhysicsOutput, PhysicsParticle};
        assert_eq!(physics_summary(None), "none");
        let group = PhysicsGroup {
            inputs: vec![PhysicsInput::default(); 3],
            outputs: vec![PhysicsOutput::default(); 2],
            particles: vec![PhysicsParticle::default(); 4],
            ..PhysicsGroup::default()
        };
        let physics = Physics { groups: vec![group.clone(), group], ..Physics::default() };
        assert_eq!(physics_summary(Some(&physics)), "2 groups, 6 inputs, 4 outputs, 8 particles");
    }

    #[test]
    fn a_clean_report_says_so() {
        let mut out = Vec::new();
        print_report(&mut out, &LoadReport::new()).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "\nLoaded cleanly.\n");
    }

    #[test]
    fn empty_controller_and_fade_lists_are_stated_not_omitted() {
        let mut inventory = a2d_import::CubismInventory {
            unity_revision: String::new(),
            object_count: 0,
            moc: None,
            textures: vec![],
            motions: vec![],
            animator_controllers: vec![],
            game_objects: 0,
            parameters: 0,
            parts: 0,
            drawables: 0,
            fade_sources: vec![],
            physics: None,
        };
        let mut out = Vec::new();
        print_controllers_and_sources(&mut out, &inventory).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains(
                "animator controllers:
    (none: no AnimatorController"
            ),
            "{text}"
        );
        assert!(
            text.contains(
                "original motion sources:
    (none: no fade motion entry names a .motion3.json)"
            ),
            "{text}"
        );

        inventory.animator_controllers = vec!["Ctrl".into()];
        inventory.fade_sources = vec!["a.motion3.json".into()];
        let mut out = Vec::new();
        print_controllers_and_sources(&mut out, &inventory).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains(
                "animator controllers:
    Ctrl"
            ),
            "{text}"
        );
        assert!(
            text.contains(
                "original motion sources:
    a.motion3.json"
            ),
            "{text}"
        );
        assert!(!text.contains("(none"), "{text}");
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

    #[test]
    fn a_bundle_import_does_not_repeat_its_warnings_as_recorded_ones() {
        let recorded = vec!["a curve was dropped".to_string()];
        let mut report = LoadReport::new();
        report.note("a curve was dropped");
        seed_report(&recorded, &mut report, false);
        assert_eq!(report.len(), 1, "{report}");
        assert!(!report.to_string().contains("recorded at import"), "{report}");
    }

    #[test]
    fn a_bundle_input_is_not_read_back_but_a_package_directory_is() {
        let dir = std::env::temp_dir().join(format!("a2d-origin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let bundle = dir.join("model.bundle");
        let mut bytes = b"UnityFS\0".to_vec();
        bytes.extend_from_slice(&[0u8; 32]);
        std::fs::write(&bundle, &bytes).unwrap();
        let package = dir.join("model.a2dpack");
        std::fs::create_dir_all(&package).unwrap();

        // The decision `preview` really makes, end to end: input kind to
        // whether recorded warnings are printed. Passing "from disk" for a
        // bundle would double every warning.
        let recorded = vec!["a curve was dropped".to_string()];
        for (input, expected) in [(&bundle, 0), (&package, 1)] {
            let mut report = LoadReport::new();
            seed_report(&recorded, &mut report, is_read_back(input));
            assert_eq!(report.len(), expected, "{}: {report}", input.display());
        }
        assert!(!is_read_back(&bundle));
        assert!(is_read_back(&package));
        assert!(is_read_back(&dir.join("missing")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_package_from_disk_prints_its_recorded_warnings_once() {
        let recorded = vec!["a curve was dropped".to_string()];
        let mut report = LoadReport::new();
        seed_report(&recorded, &mut report, true);
        assert_eq!(report.len(), 1);
        assert!(report.to_string().contains("recorded at import: a curve was dropped"), "{report}");
    }
}

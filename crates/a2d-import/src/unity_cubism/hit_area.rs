//! Hit areas: the drawables a click may land on.
//!
//! The Cubism Unity SDK marks a drawable as hittable with a `CubismHitDrawable`
//! component on the drawable's own `GameObject`, and binds it to the ArtMesh
//! whose drawable ID equals that `GameObject`'s name. A `CubismRaycastable` on
//! the same object says how exactly the mesh is tested.
//!
//! Layouts, from the SDK's public documentation and from real bundles:
//!
//! * `CubismHitDrawable`: after `m_Name`, one length-prefixed string, the
//!   area's `Name` (the field name is not verified; real bundles hold an empty
//!   one, so four bytes of zero).
//! * `CubismRaycastable`: after `m_Name`, one `i32` `Precision`: 0 is the
//!   bounding box, 1 the triangles.
//!
//! A bundle without the components may still come from a model whose editor
//! left a collision ArtMesh behind: the Cubism Editor names it `HitArea`
//! automatically. That is used, and reported as inferred, only when the bundle
//! has no `CubismHitDrawable` at all.
//!
//! Nothing here can fail an import: what cannot be bound is a degradation.

use std::collections::BTreeMap;

use a2d_core::ir::cubism::{HitArea, HitPrecision};
use a2d_core::Degradation;
use a2d_unity::{behaviour_body, behaviour_owner_id, behaviour_owner_name, Inventory, SerializedFile};

pub(super) const HIT_DRAWABLE: &str = "CubismHitDrawable";
const RAYCASTABLE: &str = "CubismRaycastable";
/// The ID the Cubism Editor gives a collision ArtMesh on its own.
pub(super) const EDITOR_COLLISION_ID: &str = "HitArea";

/// One `CubismHitDrawable`, as found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct HitDrawable {
    /// The component's own name field; empty stays empty.
    pub name: String,
    /// The owner `GameObject`'s path id; `None` when detached or unreadable.
    pub owner_id: Option<i64>,
    /// The owner's name, when readable.
    pub owner_name: Option<String>,
    /// Why the body could not be read in full, when it could not.
    pub problem: Option<String>,
}

/// What the bundle's hit components say, before binding to a model.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct HitScan {
    pub drawables: Vec<HitDrawable>,
    /// Each `CubismRaycastable`: its owner and its raw `Precision`, or why it
    /// could not be read.
    pub raycastables: Vec<(Option<i64>, Result<i32, String>)>,
}

/// The bound hit areas and what became of the rest.
#[derive(Debug, Default, Clone, PartialEq)]
pub(super) struct HitResolution {
    pub areas: Vec<HitArea>,
    pub degradations: Vec<Degradation>,
}

pub(super) fn scan(file: &SerializedFile, inventory: &Inventory) -> HitScan {
    let mut out = HitScan::default();
    for info in inventory.by_script(HIT_DRAWABLE) {
        let Some(object) = file.find_object(info.path_id) else { continue };
        let mut problem = None;
        let body = behaviour_body(file, object).and_then(|(mut r, _)| {
            let name = r.string()?;
            Ok((name, r.remaining()))
        });
        let name = match body {
            Ok((name, 0)) => name,
            Ok((name, left)) => {
                problem = Some(format!("{left} byte(s) follow its name, a layout this does not know"));
                name
            }
            Err(e) => {
                problem = Some(format!("its body is unreadable: {e}"));
                String::new()
            }
        };
        let owner_id = behaviour_owner_id(file, object).ok().flatten();
        let owner_name = behaviour_owner_name(file, object).ok().flatten();
        out.drawables.push(HitDrawable { name, owner_id, owner_name, problem });
    }
    for info in inventory.by_script(RAYCASTABLE) {
        let Some(object) = file.find_object(info.path_id) else { continue };
        let owner = behaviour_owner_id(file, object).ok().flatten();
        let precision = behaviour_body(file, object)
            .and_then(|(mut r, _)| r.i32())
            .map_err(|e| format!("its body is unreadable: {e}"));
        out.raycastables.push((owner, precision));
    }
    out
}

impl HitScan {
    /// Binds the components to the model's drawables by ID.
    pub fn resolve(&self, drawable_ids: &[String]) -> HitResolution {
        let mut out = HitResolution::default();
        // A raycastable that no hit drawable shares an owner with marks nothing
        // the SDK would test; it is reported, never turned into an area.
        for (owner, precision) in &self.raycastables {
            let paired = owner.is_some_and(|id| self.drawables.iter().any(|h| h.owner_id == Some(id)));
            if !paired {
                let value = match precision {
                    Ok(v) => format!("precision {v}"),
                    Err(e) => e.clone(),
                };
                out.degradations.push(Degradation::Note(format!(
                    "a {RAYCASTABLE} ({value}) has no {HIT_DRAWABLE} on its GameObject, so it marks no hit area and is not applied"
                )));
            }
        }
        if self.drawables.is_empty() {
            // Inference only when the bundle says nothing about hit areas.
            if self.raycastables.is_empty() {
                let matches = drawable_ids.iter().filter(|id| *id == EDITOR_COLLISION_ID).count();
                if let Some(index) = drawable_ids.iter().position(|id| id == EDITOR_COLLISION_ID) {
                    out.areas.push(HitArea {
                        name: EDITOR_COLLISION_ID.into(),
                        drawable: index as u32,
                        precision: HitPrecision::BoundingBox,
                    });
                    if matches > 1 {
                        out.degradations.push(Degradation::Note(format!(
                            "{matches} drawables share the id `{EDITOR_COLLISION_ID}`; the hit area is bound to the first"
                        )));
                    }
                    out.degradations.push(Degradation::Info(format!(
                        "hit area inferred from the editor's collision mesh id `{EDITOR_COLLISION_ID}`: the bundle has no {HIT_DRAWABLE}; tested as a bounding box"
                    )));
                }
            }
            return out;
        }
        let mut precisions: BTreeMap<i64, &Result<i32, String>> = BTreeMap::new();
        for (owner, precision) in &self.raycastables {
            if let Some(owner) = owner {
                precisions.entry(*owner).or_insert(precision);
            }
        }
        for hit in &self.drawables {
            let label = if hit.name.is_empty() { "(unnamed)".to_string() } else { format!("{:?}", hit.name) };
            if let Some(problem) = &hit.problem {
                out.degradations.push(Degradation::Note(format!("hit area {label}: {problem}")));
            }
            let Some(owner) = &hit.owner_name else {
                out.degradations.push(Degradation::Note(format!(
                    "hit area {label} has no readable owner GameObject, so it names no drawable and is not applied"
                )));
                continue;
            };
            let Some(index) = drawable_ids.iter().position(|id| id == owner) else {
                out.degradations.push(Degradation::MissingReference {
                    kind: "hit area drawable".into(),
                    name: format!("{owner} (owner of hit area {label}); the area is not applied"),
                });
                continue;
            };
            let matches = drawable_ids.iter().filter(|id| *id == owner).count();
            if matches > 1 {
                out.degradations.push(Degradation::Note(format!(
                    "{matches} drawables share the id {owner:?}; the hit area {label} is bound to the first"
                )));
            }
            let precision = match hit.owner_id.and_then(|id| precisions.get(&id)) {
                None => {
                    out.degradations.push(Degradation::Info(format!(
                        "hit area {owner:?} has no {RAYCASTABLE}; tested as a bounding box"
                    )));
                    HitPrecision::BoundingBox
                }
                Some(Ok(0)) => HitPrecision::BoundingBox,
                Some(Ok(1)) => HitPrecision::Triangles,
                Some(Ok(other)) => {
                    out.degradations.push(Degradation::Note(format!(
                        "hit area {owner:?} has unknown Raycastable precision {other}; tested as a bounding box"
                    )));
                    HitPrecision::BoundingBox
                }
                Some(Err(e)) => {
                    out.degradations.push(Degradation::Note(format!(
                        "hit area {owner:?} has a Raycastable that cannot be read ({e}); tested as a bounding box"
                    )));
                    HitPrecision::BoundingBox
                }
            };
            out.areas.push(HitArea { name: hit.name.clone(), drawable: index as u32, precision });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::super::drivers::tests::{behaviour, build, game_object, mono_script, put_i32, put_string};
    use super::*;

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn name_body(name: &str) -> Vec<u8> {
        let mut o = Vec::new();
        put_string(&mut o, name);
        o
    }

    fn precision_body(value: i32) -> Vec<u8> {
        let mut o = Vec::new();
        put_i32(&mut o, value);
        o
    }

    /// A bundle with one object holding a hit drawable and, when given, a
    /// raycastable with that precision value.
    fn scanned(owner: &str, hit_name: &str, raycast: Option<i32>) -> HitScan {
        let mut objects = vec![
            (1, 0, game_object(owner)),
            (10, 1, mono_script(HIT_DRAWABLE)),
            (11, 1, mono_script(RAYCASTABLE)),
            (20, 2, behaviour(1, 10, &name_body(hit_name))),
        ];
        if let Some(value) = raycast {
            objects.push((21, 2, behaviour(1, 11, &precision_body(value))));
        }
        let file = build(&objects);
        scan(&file, &Inventory::build(&file))
    }

    #[test]
    fn a_component_binds_to_the_drawable_named_like_its_owner_with_the_default_precision() {
        let found = scanned("HitArea", "", None);
        assert_eq!(found.drawables.len(), 1);
        let got = found.resolve(&ids(&["ArtMesh1", "HitArea"]));
        assert_eq!(got.areas, [HitArea { name: String::new(), drawable: 1, precision: HitPrecision::BoundingBox }]);
        assert!(got.degradations.iter().all(|d| matches!(d, Degradation::Info(_))), "{:?}", got.degradations);
    }

    #[test]
    fn a_raycastable_on_the_same_owner_sets_the_precision_and_a_name_is_kept() {
        let got = scanned("Head", "Face", Some(1)).resolve(&ids(&["Head"]));
        assert_eq!(got.areas, [HitArea { name: "Face".into(), drawable: 0, precision: HitPrecision::Triangles }]);
        let got = scanned("Head", "", Some(0)).resolve(&ids(&["Head"]));
        assert_eq!(got.areas[0].precision, HitPrecision::BoundingBox);
        assert!(got.degradations.is_empty());
    }

    #[test]
    fn a_raycastable_on_another_object_does_not_apply() {
        let file = build(&[
            (1, 0, game_object("Head")),
            (2, 0, game_object("Other")),
            (10, 1, mono_script(HIT_DRAWABLE)),
            (11, 1, mono_script(RAYCASTABLE)),
            (20, 2, behaviour(1, 10, &name_body(""))),
            (21, 2, behaviour(2, 11, &precision_body(1))),
        ]);
        let got = scan(&file, &Inventory::build(&file)).resolve(&ids(&["Head"]));
        assert_eq!(got.areas[0].precision, HitPrecision::BoundingBox);
    }

    #[test]
    fn an_unknown_precision_is_a_bounding_box_and_is_reported() {
        let got = scanned("Head", "", Some(7)).resolve(&ids(&["Head"]));
        assert_eq!(got.areas[0].precision, HitPrecision::BoundingBox);
        assert_eq!(got.degradations.len(), 1);
        assert!(matches!(&got.degradations[0], Degradation::Note(n) if n.contains("precision 7")), "{got:?}");
    }

    #[test]
    fn an_owner_that_matches_no_drawable_is_a_warning_and_no_area() {
        let got = scanned("Ghost", "", None).resolve(&ids(&["ArtMesh1", "HitArea"]));
        assert!(got.areas.is_empty());
        assert_eq!(got.degradations.len(), 1);
        assert!(
            matches!(&got.degradations[0], Degradation::MissingReference { kind, name }
                if kind == "hit area drawable" && name.contains("Ghost")),
            "{:?}",
            got.degradations
        );
    }

    #[test]
    fn a_component_with_no_owner_is_a_note() {
        let file = build(&[(10, 1, mono_script(HIT_DRAWABLE)), (20, 2, behaviour(0, 10, &name_body("")))]);
        let got = scan(&file, &Inventory::build(&file)).resolve(&ids(&["HitArea"]));
        assert!(got.areas.is_empty());
        assert!(matches!(&got.degradations[0], Degradation::Note(n) if n.contains("no readable owner")));
    }

    #[test]
    fn an_unreadable_or_longer_body_is_reported_and_still_binds() {
        let file =
            build(&[(1, 0, game_object("Head")), (10, 1, mono_script(HIT_DRAWABLE)), (20, 2, behaviour(1, 10, &[]))]);
        let got = scan(&file, &Inventory::build(&file)).resolve(&ids(&["Head"]));
        assert_eq!(got.areas.len(), 1);
        assert!(
            got.degradations.iter().any(|d| matches!(d, Degradation::Note(n) if n.contains("unreadable"))),
            "{got:?}"
        );
        let mut longer = name_body("");
        longer.extend_from_slice(&[0; 4]);
        let file = build(&[
            (1, 0, game_object("Head")),
            (10, 1, mono_script(HIT_DRAWABLE)),
            (20, 2, behaviour(1, 10, &longer)),
        ]);
        let got = scan(&file, &Inventory::build(&file)).resolve(&ids(&["Head"]));
        assert_eq!(got.areas.len(), 1);
        assert!(
            got.degradations.iter().any(|d| matches!(d, Degradation::Note(n) if n.contains("byte(s) follow"))),
            "{got:?}"
        );
    }

    #[test]
    fn with_no_component_the_editors_collision_mesh_is_inferred_as_information() {
        let file = build(&[(10, 1, mono_script("CubismParameterStore"))]);
        let found = scan(&file, &Inventory::build(&file));
        let got = found.resolve(&ids(&["ArtMesh1", "HitArea", "ArtMesh2"]));
        assert_eq!(got.areas, [HitArea { name: "HitArea".into(), drawable: 1, precision: HitPrecision::BoundingBox }]);
        assert_eq!(got.degradations.len(), 1);
        let Degradation::Info(text) = &got.degradations[0] else { panic!("{:?}", got.degradations) };
        assert!(text.contains("hit area inferred from the editor's collision mesh id `HitArea`"), "{text}");
        // Nothing to infer without such a drawable, and nothing is said.
        let none = found.resolve(&ids(&["ArtMesh1", "HitArea2"]));
        assert!(none.areas.is_empty() && none.degradations.is_empty());
        // The ID must match exactly.
        assert!(found.resolve(&ids(&["hitarea"])).areas.is_empty());
    }

    #[test]
    fn a_raycastable_with_no_hit_drawable_is_a_note_and_blocks_inference() {
        let file = build(&[
            (1, 0, game_object("HitArea")),
            (11, 1, mono_script(RAYCASTABLE)),
            (21, 2, behaviour(1, 11, &precision_body(1))),
        ]);
        let got = scan(&file, &Inventory::build(&file)).resolve(&ids(&["HitArea"]));
        assert!(got.areas.is_empty());
        assert_eq!(got.degradations.len(), 1, "{got:?}");
        assert!(matches!(&got.degradations[0], Degradation::Note(n) if n.contains("no CubismHitDrawable")), "{got:?}");
    }

    #[test]
    fn a_hit_drawable_without_a_raycastable_says_it_is_a_bounding_box_by_default() {
        let got = scanned("Head", "", None).resolve(&ids(&["Head"]));
        assert_eq!(got.areas[0].precision, HitPrecision::BoundingBox);
        assert_eq!(got.degradations.len(), 1);
        assert!(matches!(&got.degradations[0], Degradation::Info(n) if n.contains("no CubismRaycastable")), "{got:?}");
    }

    #[test]
    fn a_duplicated_drawable_id_is_reported_when_bound() {
        let got = scanned("Head", "", Some(0)).resolve(&ids(&["Head", "Other", "Head"]));
        assert_eq!(got.areas[0].drawable, 0);
        assert_eq!(got.degradations.len(), 1, "{got:?}");
        assert!(matches!(&got.degradations[0], Degradation::Note(n) if n.contains("2 drawables share")), "{got:?}");
        let file = build(&[(10, 1, mono_script("CubismParameterStore"))]);
        let got = scan(&file, &Inventory::build(&file)).resolve(&ids(&["HitArea", "HitArea"]));
        assert_eq!(got.areas.len(), 1);
        assert!(got.degradations.iter().any(|d| matches!(d, Degradation::Note(n) if n.contains("share the id"))));
        assert!(got.degradations.iter().any(|d| matches!(d, Degradation::Info(_))));
    }

    #[test]
    fn a_bundle_with_components_never_infers_even_when_none_binds() {
        let got = scanned("Ghost", "", None).resolve(&ids(&["HitArea"]));
        assert!(got.areas.is_empty());
        assert!(got.degradations.iter().all(|d| !matches!(d, Degradation::Info(_))));
    }
}

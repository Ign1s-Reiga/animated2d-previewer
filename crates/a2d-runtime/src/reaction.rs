//! What a character does when it is clicked.
//!
//! Pure policy, no state: [`reaction_candidates`] ranks the animations a click
//! could trigger and [`pick_reaction`] chooses one. The caller owns the random
//! source (usually an [`Rng`](crate::Rng)) and remembers what it played last, so
//! the same inputs and the same draws always give the same answer.

use a2d_core::AnimationInfo;

/// Splits a name into lowercase words: runs of letters and digits.
fn words(name: &str) -> Vec<String> {
    name.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).collect()
}

/// Whether the words of `phrase` appear, in order and next to each other, among
/// the words of `name`, ignoring case.
///
/// Words are runs of letters and digits; anything else separates them. So
/// `Tap_Head` has the words `tap` and `head`; `hitarea_x` has `hitarea` and
/// `x` (so a hit area named `HitArea` matches it); `forehead` has only itself
/// (so `head` does not); digits glue, so `touch2` is not `touch`. A phrase with
/// separators of its own matches as a run: `head_box` is in `touch_head_box`
/// and `D_REF.HEAD` is in `tap_d_ref_head`, but `head_box` is not in
/// `head_x_box`. A phrase with no words matches nothing.
fn has_word(name: &str, phrase: &str) -> bool {
    let (name, phrase) = (words(name), words(phrase));
    !phrase.is_empty() && name.windows(phrase.len()).any(|w| w == phrase.as_slice())
}

/// The animations a click may play, from the first non-empty priority tier.
///
/// `standing` (the resting animation) is never a candidate. Tiers:
///
/// 1. with `hit` given: animations whose name has the hit area's name as a
///    whole word (see [`has_word`]);
/// 2. animations that carry a source hand-back (`AnimationInfo::then`), the
///    shape of an emotion motion that returns to the default by itself;
/// 3. animations with `tap`, `touch` or `click` as a whole word;
/// 4. otherwise nothing: the character does not react.
///
/// A hit that matches nothing falls through to tier 2, so a hit area without
/// a dedicated motion still gets a reaction. Candidates keep the order of
/// `animations`.
pub fn reaction_candidates<'a>(hit: Option<&str>, animations: &'a [AnimationInfo], standing: &str) -> Vec<&'a str> {
    let pick = |keep: &dyn Fn(&AnimationInfo) -> bool| -> Vec<&'a str> {
        animations.iter().filter(|a| a.name != standing && keep(a)).map(|a| a.name.as_str()).collect()
    };
    if let Some(h) = hit {
        let by_hit = pick(&|a| has_word(&a.name, h));
        if !by_hit.is_empty() {
            return by_hit;
        }
    }
    let handed_back = pick(&|a| a.then.is_some());
    if !handed_back.is_empty() {
        return handed_back;
    }
    pick(&|a| ["tap", "touch", "click"].iter().any(|w| has_word(&a.name, w)))
}

/// Picks one of `candidates` uniformly, never `last` while another exists.
///
/// `rng(n)` must return a value in `0..n`; an out-of-range value is reduced
/// modulo `n`, so a misbehaving source cannot panic. With a single candidate
/// that candidate is returned even if it is `last`: repeating beats doing
/// nothing. No candidates gives `None`.
pub fn pick_reaction<'a>(
    candidates: &[&'a str],
    last: Option<&str>,
    mut rng: impl FnMut(usize) -> usize,
) -> Option<&'a str> {
    let others: Vec<&'a str> = candidates.iter().copied().filter(|c| Some(*c) != last).collect();
    // Fewer than two candidates, or only copies of `last`: nothing else to offer.
    let pool: &[&'a str] = if candidates.len() > 1 && !others.is_empty() { &others } else { candidates };
    if pool.is_empty() {
        return None;
    }
    pool.get(rng(pool.len()) % pool.len()).copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use a2d_core::Handover;

    fn plain(name: &str) -> AnimationInfo {
        AnimationInfo { name: name.into(), duration: 1.0, then: None }
    }

    fn back(name: &str) -> AnimationInfo {
        AnimationInfo {
            name: name.into(),
            duration: 1.0,
            then: Some(Handover { animation: "stand".into(), at: 0.8, mix: 0.2 }),
        }
    }

    #[test]
    fn a_hit_name_wins_over_every_later_tier() {
        let a = [plain("stand"), plain("tap_body"), back("happy"), plain("touch_Head")];
        assert_eq!(reaction_candidates(Some("Head"), &a, "stand"), vec!["touch_Head"]);
    }

    #[test]
    fn hand_backs_win_over_tap_names() {
        let a = [plain("stand"), plain("tap_body"), back("happy")];
        // The hit matches nothing, so the next tier answers.
        assert_eq!(reaction_candidates(Some("Head"), &a, "stand"), vec!["happy"]);
        assert_eq!(reaction_candidates(None, &a, "stand"), vec!["happy"]);
    }

    #[test]
    fn tap_touch_and_click_are_the_last_tier() {
        let a = [plain("stand"), plain("run"), plain("Tap_01"), plain("touch"), plain("on-click")];
        assert_eq!(reaction_candidates(None, &a, "stand"), vec!["Tap_01", "touch", "on-click"]);
    }

    #[test]
    fn nothing_matching_means_no_reaction() {
        let a = [plain("stand"), plain("run")];
        assert!(reaction_candidates(Some("Head"), &a, "stand").is_empty());
        assert!(reaction_candidates(None, &a, "stand").is_empty());
        assert!(reaction_candidates(None, &[], "stand").is_empty());
        assert!(reaction_candidates(Some("Head"), &[], "stand").is_empty());
    }

    #[test]
    fn matching_is_by_whole_word_and_ignores_case() {
        let a = [plain("stand"), plain("forehead_pat"), plain("HEAD-pat"), plain("headband")];
        assert_eq!(reaction_candidates(Some("head"), &a, "stand"), vec!["HEAD-pat"]);
        // A separator-delimited word matches even with more words around it.
        let b = [plain("stand"), plain("hitarea_x"), plain("touch2")];
        assert_eq!(reaction_candidates(Some("HitArea"), &b, "stand"), vec!["hitarea_x"]);
        // Digits glue to the word, so `touch2` is not `touch`.
        assert!(reaction_candidates(None, &b, "stand").is_empty());
    }

    #[test]
    fn a_hit_name_with_separators_matches_as_a_contiguous_run() {
        let a = [plain("stand"), plain("touch_head_box"), plain("head_x_box"), plain("head"), plain("tap_d_ref_head")];
        assert_eq!(reaction_candidates(Some("head_box"), &a, "stand"), vec!["touch_head_box"]);
        assert_eq!(reaction_candidates(Some("D_REF.HEAD"), &a, "stand"), vec!["tap_d_ref_head"]);
        let b = [plain("stand"), plain("forehead_box")];
        assert!(reaction_candidates(Some("head"), &b, "stand").is_empty());
        assert!(reaction_candidates(Some("head_box"), &b, "stand").is_empty());
    }

    #[test]
    fn an_empty_hit_name_matches_nothing() {
        let a = [plain("stand"), plain("a_b")];
        assert!(reaction_candidates(Some(""), &a, "stand").is_empty());
    }

    #[test]
    fn the_standing_animation_is_never_a_candidate() {
        let a = [back("stand"), plain("tap_stand_x"), plain("tap")];
        assert_eq!(reaction_candidates(None, &a, "stand"), vec!["tap_stand_x", "tap"]);
        assert!(reaction_candidates(Some("tap"), &[back("tap")], "tap").is_empty());
    }

    #[test]
    fn pick_never_repeats_last_with_several_candidates() {
        let c = ["a", "b", "c"];
        for last in c {
            for r in 0..9 {
                assert_ne!(pick_reaction(&c, Some(last), |n| r % n), Some(last));
            }
        }
    }

    #[test]
    fn pick_draws_from_the_rest_and_may_repeat_a_single_candidate() {
        let c = ["a", "b", "c"];
        assert_eq!(pick_reaction(&c, Some("a"), |_| 0), Some("b"));
        assert_eq!(pick_reaction(&c, Some("a"), |_| 1), Some("c"));
        assert_eq!(pick_reaction(&c, None, |_| 0), Some("a"));
        assert_eq!(pick_reaction(&["a"], Some("a"), |_| 0), Some("a"));
        assert_eq!(pick_reaction(&[], None, |_| 0), None);
        // An out-of-range draw cannot panic.
        assert_eq!(pick_reaction(&c, None, |_| 100), Some("b"));
    }

    #[test]
    fn pick_is_deterministic_with_a_seeded_rng() {
        let c = ["a", "b", "c", "d"];
        let run = |seed| {
            let mut rng = crate::Rng::new(seed);
            let mut last: Option<&str> = None;
            (0..8)
                .map(|_| {
                    let p = pick_reaction(&c, last, |n| rng.below(n));
                    last = p;
                    p
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(run(7), run(7));
        assert!(run(7).windows(2).all(|w| w[0] != w[1]));
    }
}

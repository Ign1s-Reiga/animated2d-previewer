//! Degradation reporting.
//!
//! Spec §16: never silently discard unsupported data. Every loader returns a
//! [`LoadReport`] alongside the model, and every CLI surface prints it. A
//! warning that no surface prints is a bug.

use std::fmt;

/// One thing that was dropped, approximated, or could not be resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Degradation {
    /// A timeline type the decoder recognised but does not translate.
    UnsupportedTimeline { animation: String, kind: String },
    /// A constraint present in the source that the runtime will not evaluate.
    UnsupportedConstraint { name: String, kind: String },
    /// An attachment type that is parsed but not rendered.
    UnsupportedAttachment { slot: String, name: String, kind: String },
    /// A blend mode with no renderer equivalent; the fallback is named.
    UnsupportedBlendMode { slot: String, requested: String, fallback: String },
    /// A named asset the source referenced but the package does not contain.
    MissingReference { kind: String, name: String },
    /// A field was present but held a value outside the range the format allows.
    ClampedValue { context: String, field: String, detail: String },
    /// Anything else worth telling the user, with no dedicated variant yet.
    Note(String),
}

impl fmt::Display for Degradation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Degradation::UnsupportedTimeline { animation, kind } => {
                write!(f, "{kind} timeline unsupported in animation {animation:?}")
            }
            Degradation::UnsupportedConstraint { name, kind } => {
                write!(f, "{kind} constraint {name:?} unsupported")
            }
            Degradation::UnsupportedAttachment { slot, name, kind } => {
                write!(f, "{kind} attachment {name:?} on slot {slot:?} unsupported")
            }
            Degradation::UnsupportedBlendMode { slot, requested, fallback } => {
                write!(f, "blend mode {requested:?} on slot {slot:?} unsupported, using {fallback:?}")
            }
            Degradation::MissingReference { kind, name } => write!(f, "missing {kind}: {name}"),
            Degradation::ClampedValue { context, field, detail } => {
                write!(f, "clamped {field} in {context}: {detail}")
            }
            Degradation::Note(msg) => f.write_str(msg),
        }
    }
}

/// Collected degradations from one load.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoadReport {
    warnings: Vec<Degradation>,
}

impl LoadReport {
    pub fn new() -> Self {
        LoadReport::default()
    }

    pub fn warn(&mut self, d: Degradation) {
        // Decoders hit the same unsupported feature once per keyframe; collapsing
        // here keeps the report readable without callers having to deduplicate.
        if !self.warnings.contains(&d) {
            self.warnings.push(d);
        }
    }

    pub fn note(&mut self, message: impl Into<String>) {
        self.warn(Degradation::Note(message.into()));
    }

    /// Folds another report into this one, preserving order and deduplicating.
    pub fn absorb(&mut self, other: LoadReport) {
        for d in other.warnings {
            self.warn(d);
        }
    }

    pub fn warnings(&self) -> &[Degradation] {
        &self.warnings
    }

    pub fn is_empty(&self) -> bool {
        self.warnings.is_empty()
    }

    pub fn len(&self) -> usize {
        self.warnings.len()
    }
}

impl fmt::Display for LoadReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.warnings.is_empty() {
            return f.write_str("Loaded cleanly.");
        }
        writeln!(f, "Loaded with warnings:")?;
        for (i, w) in self.warnings.iter().enumerate() {
            if i + 1 == self.warnings.len() {
                write!(f, "- {w}")?;
            } else {
                writeln!(f, "- {w}")?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_report_says_so() {
        assert!(LoadReport::new().is_empty());
        assert_eq!(LoadReport::new().to_string(), "Loaded cleanly.");
    }

    #[test]
    fn identical_warnings_are_collapsed() {
        let mut r = LoadReport::new();
        for _ in 0..5 {
            r.warn(Degradation::UnsupportedTimeline { animation: "idle".into(), kind: "path".into() });
        }
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn distinct_warnings_are_all_kept_in_order() {
        let mut r = LoadReport::new();
        r.warn(Degradation::UnsupportedTimeline { animation: "idle".into(), kind: "transform".into() });
        r.warn(Degradation::MissingReference { kind: "expression".into(), name: "smile_02".into() });
        assert_eq!(r.len(), 2);
        assert_eq!(
            r.to_string(),
            "Loaded with warnings:\n\
             - transform timeline unsupported in animation \"idle\"\n\
             - missing expression: smile_02"
        );
    }

    #[test]
    fn absorb_merges_and_deduplicates() {
        let mut a = LoadReport::new();
        a.note("one");
        let mut b = LoadReport::new();
        b.note("one");
        b.note("two");
        a.absorb(b);
        assert_eq!(a.len(), 2);
    }
}

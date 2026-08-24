//! A field reader that tracks which keys it consumed.
//!
//! Spec §16 forbids silently discarding data. Rather than trusting that every
//! decoder branch remembered to warn, every JSON object is read through
//! [`Fields`], and [`Fields::finish`] reports whatever was left over. When a
//! real asset uses an exporter key this decoder has never seen, `inspect` says
//! so instead of quietly producing a wrong model.

use a2d_core::{DecodeError, LoadReport};
use serde_json::{Map, Value};

/// Wraps a JSON object and records the keys that were read.
#[derive(Debug)]
pub struct Fields<'a> {
    map: Option<&'a Map<String, Value>>,
    seen: Vec<&'static str>,
    /// Path used in error and warning messages, e.g. `bones[3]`.
    context: String,
}

impl<'a> Fields<'a> {
    pub fn new(value: &'a Value, context: impl Into<String>) -> Fields<'a> {
        Fields { map: value.as_object(), seen: Vec::new(), context: context.into() }
    }

    /// An empty object, for optional sections that were absent entirely.
    pub fn empty(context: impl Into<String>) -> Fields<'a> {
        Fields { map: None, seen: Vec::new(), context: context.into() }
    }

    pub fn context(&self) -> &str {
        &self.context
    }

    pub fn is_object(&self) -> bool {
        self.map.is_some()
    }

    /// Reads a key and marks it consumed.
    pub fn get(&mut self, key: &'static str) -> Option<&'a Value> {
        if !self.seen.contains(&key) {
            self.seen.push(key);
        }
        self.map?.get(key).filter(|v| !v.is_null())
    }

    /// Marks a key consumed without reading it, for keys handled elsewhere.
    pub fn mark(&mut self, key: &'static str) {
        if !self.seen.contains(&key) {
            self.seen.push(key);
        }
    }

    pub fn f32(&mut self, key: &'static str, default: f32) -> f32 {
        self.get(key).and_then(as_f32).unwrap_or(default)
    }

    pub fn opt_f32(&mut self, key: &'static str) -> Option<f32> {
        self.get(key).and_then(as_f32)
    }

    pub fn i32(&mut self, key: &'static str, default: i32) -> i32 {
        self.get(key).and_then(|v| v.as_i64()).map(|v| v as i32).unwrap_or(default)
    }

    pub fn u32(&mut self, key: &'static str, default: u32) -> u32 {
        self.get(key).and_then(|v| v.as_u64()).map(|v| v as u32).unwrap_or(default)
    }

    pub fn bool(&mut self, key: &'static str, default: bool) -> bool {
        self.get(key).and_then(|v| v.as_bool()).unwrap_or(default)
    }

    pub fn str(&mut self, key: &'static str) -> Option<&'a str> {
        self.get(key).and_then(|v| v.as_str())
    }

    pub fn string(&mut self, key: &'static str) -> Option<String> {
        self.str(key).map(str::to_string)
    }

    /// Reads a required string, failing with a located error when absent.
    pub fn require_str(&mut self, key: &'static str) -> Result<&'a str, DecodeError> {
        self.str(key)
            .ok_or_else(|| DecodeError::corrupt(format!("{}: missing required string field `{key}`", self.context)))
    }

    pub fn array(&mut self, key: &'static str) -> Option<&'a Vec<Value>> {
        self.get(key).and_then(|v| v.as_array())
    }

    /// Reads an array of numbers, rejecting any element that is not one.
    pub fn f32_array(&mut self, key: &'static str) -> Result<Vec<f32>, DecodeError> {
        let Some(arr) = self.array(key) else { return Ok(Vec::new()) };
        arr.iter()
            .map(|v| {
                as_f32(v).ok_or_else(|| {
                    DecodeError::corrupt(format!("{}: `{key}` contains a non-numeric element", self.context))
                })
            })
            .collect()
    }

    /// Reads an array of small non-negative integers, as used for indices.
    pub fn u16_array(&mut self, key: &'static str) -> Result<Vec<u16>, DecodeError> {
        let Some(arr) = self.array(key) else { return Ok(Vec::new()) };
        arr.iter()
            .map(|v| {
                v.as_u64().filter(|n| *n <= u16::MAX as u64).map(|n| n as u16).ok_or_else(|| {
                    DecodeError::corrupt(format!("{}: `{key}` contains an out-of-range index", self.context))
                })
            })
            .collect()
    }

    /// Iterates the object's entries without marking anything consumed.
    ///
    /// Used for the name-keyed maps Spine uses for skins, events and animations.
    pub fn entries(&self) -> impl Iterator<Item = (&'a String, &'a Value)> {
        self.map.into_iter().flat_map(|m| m.iter())
    }

    pub fn len(&self) -> usize {
        self.map.map_or(0, Map::len)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Reports every key that was never read.
    pub fn finish(self, report: &mut LoadReport) {
        let Some(map) = self.map else { return };
        for key in map.keys() {
            if !self.seen.iter().any(|s| s == key) {
                report.note(format!("{}: ignored unknown key `{key}`", self.context));
            }
        }
    }
}

/// Accepts both JSON numbers and the numeric strings some exporters emit.
pub fn as_f32(v: &Value) -> Option<f32> {
    match v {
        Value::Number(n) => n.as_f64().map(|f| f as f32),
        Value::String(s) => s.parse::<f32>().ok(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn typed_getters_read_values_and_apply_defaults() {
        let v = json!({"x": 1.5, "n": 7, "flag": true, "s": "hip"});
        let mut f = Fields::new(&v, "obj");
        assert_eq!(f.f32("x", 0.0), 1.5);
        assert_eq!(f.f32("missing", -3.0), -3.0);
        assert_eq!(f.i32("n", 0), 7);
        assert!(f.bool("flag", false));
        assert_eq!(f.str("s"), Some("hip"));
        assert_eq!(f.str("nope"), None);
    }

    #[test]
    fn numeric_strings_are_accepted_as_numbers() {
        let v = json!({"x": "2.5"});
        let mut f = Fields::new(&v, "obj");
        assert_eq!(f.f32("x", 0.0), 2.5);
    }

    #[test]
    fn null_is_treated_as_absent() {
        let v = json!({"x": null});
        let mut f = Fields::new(&v, "obj");
        assert_eq!(f.f32("x", 9.0), 9.0);
        assert_eq!(f.str("x"), None);
    }

    #[test]
    fn unread_keys_are_reported() {
        let v = json!({"known": 1, "surprise": 2, "another": 3});
        let mut f = Fields::new(&v, "bones[0]");
        f.f32("known", 0.0);
        let mut report = LoadReport::new();
        f.finish(&mut report);
        let text = report.to_string();
        assert!(text.contains("`surprise`"), "{text}");
        assert!(text.contains("`another`"), "{text}");
        // The consumed key must not be listed. Matched with backticks because
        // the message itself contains the word "unknown".
        assert!(!text.contains("`known`"), "{text}");
        assert!(text.contains("bones[0]"), "{text}");
    }

    #[test]
    fn a_key_read_but_absent_still_counts_as_consumed() {
        let v = json!({});
        let mut f = Fields::new(&v, "obj");
        f.f32("x", 0.0);
        let mut report = LoadReport::new();
        f.finish(&mut report);
        assert!(report.is_empty());
    }

    #[test]
    fn mark_suppresses_a_report_for_a_key_handled_elsewhere() {
        let v = json!({"vertices": [1, 2]});
        let mut f = Fields::new(&v, "obj");
        f.mark("vertices");
        let mut report = LoadReport::new();
        f.finish(&mut report);
        assert!(report.is_empty());
    }

    #[test]
    fn required_strings_produce_a_located_error_when_missing() {
        let v = json!({});
        let mut f = Fields::new(&v, "slots[2]");
        let err = f.require_str("name").unwrap_err();
        assert!(err.to_string().contains("slots[2]"), "{err}");
        assert!(err.to_string().contains("name"), "{err}");
    }

    #[test]
    fn numeric_arrays_reject_non_numeric_elements() {
        let v = json!({"vertices": [1.0, "two", 3.0]});
        let mut f = Fields::new(&v, "obj");
        assert!(f.f32_array("vertices").is_err());
    }

    #[test]
    fn numeric_arrays_accept_numeric_strings() {
        let v = json!({"vertices": [1.0, "2", 3]});
        let mut f = Fields::new(&v, "obj");
        assert_eq!(f.f32_array("vertices").unwrap(), vec![1.0, 2.0, 3.0]);
    }

    #[test]
    fn index_arrays_reject_out_of_range_values() {
        let v = json!({"triangles": [0, 70000]});
        let mut f = Fields::new(&v, "obj");
        assert!(f.u16_array("triangles").is_err());
    }

    #[test]
    fn a_missing_array_reads_as_empty_not_an_error() {
        let v = json!({});
        let mut f = Fields::new(&v, "obj");
        assert!(f.f32_array("vertices").unwrap().is_empty());
    }

    #[test]
    fn a_non_object_value_yields_an_empty_field_set() {
        let v = json!([1, 2, 3]);
        let mut f = Fields::new(&v, "obj");
        assert!(!f.is_object());
        assert_eq!(f.f32("x", 4.0), 4.0);
        assert!(f.is_empty());
    }

    #[test]
    fn entries_iterates_name_keyed_maps() {
        let v = json!({"idle": 1, "walk": 2});
        let f = Fields::new(&v, "animations");
        let mut names: Vec<&str> = f.entries().map(|(k, _)| k.as_str()).collect();
        names.sort_unstable();
        assert_eq!(names, vec!["idle", "walk"]);
    }
}

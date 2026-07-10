//! Typed context path (`Path`) — parsed, validated IR for the `$.a.b` /
//! RFC 9535-style bracket path syntax used throughout flow.ir
//! (`Expr::Path.at`, `Node::Fanout.bind`/`.out`, `Node::Assign.at`,
//! `Node::Try.err_at`'s inner `Expr::Path`, ...).
//!
//! `Path` is the single authority for path syntax: parsing happens exactly
//! once, at [`FromStr`]/[`Deserialize`] time (parse-don't-validate). `Node`
//! / `Expr` fields carrying a path store an already-parsed `Path`, so
//! evaluation (`read`/`write`) walks the same `Vec<Segment>` without ever
//! re-parsing a string.
//!
//! # Syntax
//!
//! - `$` — root path (empty segment list); [`Path::read`] returns the whole
//!   ctx, [`Path::write`] replaces it wholesale.
//! - `$.a.b.c` — dot-separated object-key segments.
//! - `$.a["p.md"]` / `$["x.y"]` — RFC 9535 (JSONPath) style bracket segments
//!   for keys containing a literal `.` (double-quoted, no escape support —
//!   a key containing `"` cannot be represented in bracket form). Bracket
//!   segments may chain directly (`$.a["x"]["y"]`) or be followed by a dot
//!   segment (`$["x.y"].inner`).
//!
//! No array-index support (MVP scope, unchanged from the pre-`Path` parser).
//!
//! # Uniform rejections (all [`PathParseError`], surfaced as
//! [`EvalError::InvalidPath`] by the `read_path` / `write_path` compat
//! wrappers, or as a deserialize error when a `Path` field is parsed from
//! JSON)
//!
//! - anything not starting with `$` followed immediately by `.`, `[`, or
//!   end-of-string — so `$foo` is rejected (it used to be silently accepted
//!   as a 1-segment dot path: `"$foo".strip_prefix("$.")` fails, and the old
//!   parser's `strip_prefix('$')` fallback let it through).
//! - any empty dot segment: `$.`, `$.a.`, `$.a..b` — the old write-side
//!   parser silently *dropped* empty segments (`.filter(|s| !s.is_empty())`)
//!   while the old read-side parser only failed if the ctx happened to have
//!   no key `""` at that position (`EvalError::PathNotFound`, not
//!   `EvalError::InvalidPath`) — both were symptoms of the same missing
//!   parse-time check, and both are now rejected uniformly, up front.
//! - the existing bracket-notation rejections (unterminated bracket, missing
//!   `"` after `[`, empty key, empty `[""]`, a bracket segment directly
//!   followed by an unseparated plain segment).

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use thiserror::Error;

use crate::EvalError;

/// One resolved path segment. Currently only object-key segments are
/// supported; the variant is kept non-exhaustive-in-spirit (private, single
/// arm) so a future `Index(usize)` (array-index support) is a pure addition.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Segment {
    /// An object-key segment (from either dot form or a quoted bracket
    /// segment) — never empty (the parser rejects empty segments).
    Key(String),
}

/// A parsed, validated context path — the canonical IR for the flow.ir
/// `$.a.b` / RFC 9535-style bracket path syntax. The full syntax and
/// rejection rules are documented on [`Path::read`] / [`Path::write`] and
/// the `FromStr` implementation below.
///
/// Illegal path syntax cannot be represented by this type: the only way to
/// construct a `Path` is [`FromStr::from_str`] (equivalently `str::parse`)
/// or [`Deserialize`], both of which reject malformed input up front. Once
/// you hold a `Path`, [`Path::read`] / [`Path::write`] never re-derive or
/// re-validate the segment list.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Path(Vec<Segment>);

/// Error returned by [`Path::from_str`] (and therefore surfaced through
/// `Path`'s [`Deserialize`] impl, and through the `read_path` / `write_path`
/// compat wrappers as [`EvalError::InvalidPath`]) on malformed path syntax.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("invalid path syntax '{path}': {reason}")]
pub struct PathParseError {
    /// The original (unparseable) path string.
    pub path: String,
    /// Human-readable reason the path was rejected.
    pub reason: String,
}

impl PathParseError {
    fn new(path: &str, reason: &str) -> Self {
        Self {
            path: path.to_string(),
            reason: reason.to_string(),
        }
    }
}

impl FromStr for Path {
    type Err = PathParseError;

    fn from_str(path: &str) -> Result<Self, Self::Err> {
        let rest = path
            .strip_prefix('$')
            .ok_or_else(|| PathParseError::new(path, "path must start with '$'"))?;
        if rest.is_empty() {
            // Bare `$` — root path, no segments.
            return Ok(Path(Vec::new()));
        }
        let body = match rest.as_bytes()[0] {
            b'.' => {
                let after_dot = &rest[1..];
                if after_dot.is_empty() {
                    return Err(PathParseError::new(
                        path,
                        "trailing '.' with no segment after it",
                    ));
                }
                after_dot
            }
            b'[' => rest,
            _ => {
                return Err(PathParseError::new(
                    path,
                    "expected '.', '[', or end-of-string right after '$'",
                ))
            }
        };
        let segments = if body.contains('[') {
            parse_bracket_segments(body, path)?
        } else {
            parse_dot_segments(body, path)?
        };
        Ok(Path(segments.into_iter().map(Segment::Key).collect()))
    }
}

/// Split a (already `$`/`$.`-stripped) bracket-free body on `.`, rejecting
/// any empty segment (leading/trailing/consecutive dots).
fn parse_dot_segments(body: &str, original: &str) -> Result<Vec<String>, PathParseError> {
    let mut segments = Vec::new();
    for part in body.split('.') {
        if part.is_empty() {
            return Err(PathParseError::new(
                original,
                "empty path segment (leading, trailing, or consecutive '.')",
            ));
        }
        segments.push(part.to_string());
    }
    Ok(segments)
}

/// Parse a (`$`/`$.`-stripped) body containing at least one `[` into its
/// object-key segments. Supports:
///
/// - plain segment: any run of chars excluding `.` and `[`, non-empty.
/// - bracket segment: `["<name>"]`, where `<name>` is one or more chars
///   excluding `"` (no escape support — a key containing `"` is rejected).
/// - plain segments are `.`-separated; a bracket segment may follow
///   directly after the previous segment (`a["x"]`) or after a `.`
///   (`a.["x"]`), and a bracket segment may itself be followed directly by
///   another bracket (`a["x"]["y"]`) or by a `.` before the next plain
///   segment (`a["x"].b`).
///
/// Any malformed sequence (unterminated bracket, missing quote, empty key,
/// empty segment, bracket directly followed by an unseparated plain
/// segment, ...) raises `PathParseError` — this parser never silently
/// misparses.
fn parse_bracket_segments(body: &str, original: &str) -> Result<Vec<String>, PathParseError> {
    fn invalid(original: &str, reason: &str) -> PathParseError {
        PathParseError::new(original, reason)
    }

    let bytes = body.as_bytes();
    let len = bytes.len();
    let mut segments = Vec::new();
    let mut i = 0usize;
    // true at path start and immediately after a `.`: the next byte must
    // begin a new segment (plain or bracket), not another `.` or EOF.
    let mut expect_segment_start = true;

    while i < len {
        match bytes[i] {
            b'[' => {
                if i + 1 >= len || bytes[i + 1] != b'"' {
                    return Err(invalid(original, "expected '\"' after '['"));
                }
                let name_start = i + 2;
                let mut j = name_start;
                while j < len && bytes[j] != b'"' {
                    j += 1;
                }
                if j >= len {
                    return Err(invalid(original, "unterminated bracket segment"));
                }
                let name = &body[name_start..j];
                if name.is_empty() {
                    return Err(invalid(original, "empty bracket key"));
                }
                if j + 1 >= len || bytes[j + 1] != b']' {
                    return Err(invalid(original, "missing closing ']' after key"));
                }
                segments.push(name.to_string());
                i = j + 2;
                expect_segment_start = false;
                // Only `.` or another `[` (or EOF) may directly follow a
                // bracket segment — a bare plain-segment continuation
                // (`a["x"]b`) is ambiguous and rejected.
                if i < len && bytes[i] != b'.' && bytes[i] != b'[' {
                    return Err(invalid(
                        original,
                        "expected '.' or '[' after bracket segment",
                    ));
                }
            }
            b'.' => {
                if expect_segment_start {
                    return Err(invalid(original, "empty path segment"));
                }
                i += 1;
                expect_segment_start = true;
                if i >= len {
                    return Err(invalid(original, "empty path segment"));
                }
            }
            _ => {
                let start = i;
                while i < len && bytes[i] != b'.' && bytes[i] != b'[' {
                    i += 1;
                }
                segments.push(body[start..i].to_string());
                expect_segment_start = false;
            }
        }
    }

    if expect_segment_start {
        return Err(invalid(original, "empty path segment"));
    }

    Ok(segments)
}

/// A key is safe to render in dot form iff it cannot be confused with a
/// path delimiter: no literal `.`, `[`, or `]`. Anything else (including a
/// key containing `"`, which dot form has no trouble with) still round-trips
/// through dot form.
fn is_dot_safe(key: &str) -> bool {
    !key.is_empty() && !key.contains(['.', '[', ']'])
}

impl fmt::Display for Path {
    /// Canonical string form: `$` + `.key` for each identifier-safe segment,
    /// `["key"]` bracket form otherwise. `Path::from_str(path.to_string())`
    /// always re-parses to an equal `Path` (round-trip law) — the canonical
    /// form may normalize the *representation* (e.g. a segment reachable
    /// only via dot form on the way in is still rendered via dot form; a
    /// segment that required bracket form on the way in is rendered via
    /// bracket form) without changing the parsed segment list.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "$")?;
        for Segment::Key(key) in &self.0 {
            if is_dot_safe(key) {
                write!(f, ".{key}")?;
            } else {
                write!(f, "[\"{key}\"]")?;
            }
        }
        Ok(())
    }
}

impl Serialize for Path {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Path {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        s.parse::<Path>().map_err(serde::de::Error::custom)
    }
}

impl Path {
    /// Read the value this path resolves to inside `ctx`.
    ///
    /// The root path (`$`) resolves to `ctx` itself. A missing key along the
    /// way raises [`EvalError::PathNotFound`] — malformed *syntax* is
    /// rejected earlier, at parse time, so `read` can never raise
    /// [`EvalError::InvalidPath`].
    pub fn read<'a>(&self, ctx: &'a Value) -> Result<&'a Value, EvalError> {
        let mut cur = ctx;
        for Segment::Key(key) in &self.0 {
            cur = cur
                .get(key)
                .ok_or_else(|| EvalError::PathNotFound(self.to_string()))?;
        }
        Ok(cur)
    }

    /// Write `value` at the location this path resolves to inside `ctx`,
    /// mutating `ctx` in place.
    ///
    /// The root path (`$`) replaces `ctx` wholesale. Missing intermediate
    /// objects along the way are created automatically (a `null` — or
    /// altogether absent — intermediate promotes to an empty object, same
    /// as before this type existed). If an intermediate segment already
    /// holds a concrete non-object value (a string, number, bool, or
    /// array), the write is rejected with [`EvalError::TypeError`] instead
    /// of silently clobbering it; `ctx` is left byte-for-byte unmodified in
    /// that case (a rejected write never partially applies, because every
    /// intermediate object promotion this method performs only ever touches
    /// a freshly-created — previously `null`/absent — subtree, which by
    /// construction cannot itself contain a pre-existing conflicting value
    /// further down).
    pub fn write(&self, ctx: &mut Value, value: Value) -> Result<(), EvalError> {
        if self.0.is_empty() {
            *ctx = value;
            return Ok(());
        }
        write_recursive(ctx, &self.0, value, self)
    }
}

fn write_recursive(
    node: &mut Value,
    keys: &[Segment],
    value: Value,
    full_path: &Path,
) -> Result<(), EvalError> {
    let Segment::Key(key) = &keys[0];
    ensure_writable_object(node, full_path, key)?;
    let obj = node
        .as_object_mut()
        .expect("ensure_writable_object guarantees an object here");
    if keys.len() == 1 {
        obj.insert(key.clone(), value);
        Ok(())
    } else {
        let entry = obj.entry(key.clone()).or_insert(Value::Null);
        write_recursive(entry, &keys[1..], value, full_path)
    }
}

/// Ensure `node` is writable as an intermediate/leaf object slot: already an
/// object is a no-op, `null` (missing/uninitialised) promotes to an empty
/// object, anything else (string/number/bool/array) is rejected — it would
/// otherwise be silently clobbered.
fn ensure_writable_object(node: &mut Value, full_path: &Path, key: &str) -> Result<(), EvalError> {
    if node.is_object() {
        return Ok(());
    }
    if node.is_null() {
        *node = Value::Object(serde_json::Map::new());
        return Ok(());
    }
    Err(EvalError::TypeError {
        op: "path.write".into(),
        msg: format!(
            "cannot write path '{full_path}' at segment '{key}': existing value at this \
             position is not an object ({node:?})"
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── accept/reject table ────────────────────────────────────────────

    #[test]
    fn accepts_root() {
        let p: Path = "$".parse().unwrap();
        assert_eq!(p, Path(Vec::new()));
    }

    #[test]
    fn accepts_single_dot_segment() {
        let p: Path = "$.a".parse().unwrap();
        assert_eq!(p, Path(vec![Segment::Key("a".into())]));
    }

    #[test]
    fn accepts_multi_dot_segments() {
        let p: Path = "$.a.b".parse().unwrap();
        assert_eq!(
            p,
            Path(vec![Segment::Key("a".into()), Segment::Key("b".into())])
        );
    }

    #[test]
    fn accepts_bracket_forms() {
        assert!("$.a[\"p.md\"]".parse::<Path>().is_ok());
        assert!("$[\"x.y\"]".parse::<Path>().is_ok());
        assert!("$[\"x.y\"].inner".parse::<Path>().is_ok());
        assert!("$.a[\"x\"][\"y\"]".parse::<Path>().is_ok());
    }

    #[test]
    fn rejects_missing_dollar_prefix() {
        assert!("a.b".parse::<Path>().is_err());
        assert!("".parse::<Path>().is_err());
    }

    #[test]
    fn rejects_dollar_foo_no_dot() {
        // previously silently accepted as a 1-segment dot path
        let err = "$foo".parse::<Path>().unwrap_err();
        assert_eq!(err.path, "$foo");
    }

    #[test]
    fn rejects_trailing_dot() {
        assert!("$.".parse::<Path>().is_err());
        assert!("$.a.".parse::<Path>().is_err());
    }

    #[test]
    fn rejects_empty_middle_segment() {
        assert!("$.a..b".parse::<Path>().is_err());
    }

    #[test]
    fn rejects_empty_bracket_key() {
        assert!("$.a[\"\"]".parse::<Path>().is_err());
        assert!("$.a[]".parse::<Path>().is_err());
    }

    #[test]
    fn rejects_unterminated_bracket() {
        assert!("$.a[".parse::<Path>().is_err());
        assert!("$.a[\"x".parse::<Path>().is_err());
    }

    #[test]
    fn rejects_unquoted_bracket_key() {
        assert!("$.a[p.md]".parse::<Path>().is_err());
    }

    #[test]
    fn rejects_unseparated_plain_suffix_after_bracket() {
        assert!("$.a[\"x\"]b".parse::<Path>().is_err());
    }

    // ── Display round-trip ─────────────────────────────────────────────

    #[test]
    fn display_round_trip() {
        for src in [
            "$",
            "$.a",
            "$.a.b.c",
            "$.a[\"p.md\"]",
            "$[\"x.y\"]",
            "$[\"x.y\"].inner",
            "$.a[\"x\"][\"y\"]",
        ] {
            let parsed: Path = src.parse().unwrap();
            let rendered = parsed.to_string();
            let reparsed: Path = rendered.parse().unwrap_or_else(|e| {
                panic!("canonical form '{rendered}' (from '{src}') failed to re-parse: {e}")
            });
            assert_eq!(
                parsed, reparsed,
                "round-trip mismatch for '{src}' -> '{rendered}'"
            );
        }
    }

    #[test]
    fn display_dot_safe_key_renders_dot_form() {
        let p: Path = "$.a".parse().unwrap();
        assert_eq!(p.to_string(), "$.a");
    }

    #[test]
    fn display_dotted_key_renders_bracket_form() {
        let p: Path = "$.a[\"p.md\"]".parse().unwrap();
        assert_eq!(p.to_string(), "$.a[\"p.md\"]");
    }

    // ── read / write ────────────────────────────────────────────────────

    #[test]
    fn read_root_returns_whole_ctx() {
        let p: Path = "$".parse().unwrap();
        let ctx = json!({"a": 1});
        assert_eq!(p.read(&ctx).unwrap(), &ctx);
    }

    #[test]
    fn read_missing_key_errors_path_not_found() {
        let p: Path = "$.a.missing".parse().unwrap();
        let ctx = json!({"a": {}});
        let err = p.read(&ctx).unwrap_err();
        assert!(matches!(err, EvalError::PathNotFound(_)), "{err:?}");
    }

    #[test]
    fn write_root_replaces_whole_ctx() {
        let p: Path = "$".parse().unwrap();
        let mut ctx = json!({"a": 1});
        p.write(&mut ctx, json!({"b": 2})).unwrap();
        assert_eq!(ctx, json!({"b": 2}));
    }

    #[test]
    fn write_creates_missing_intermediate_objects() {
        let p: Path = "$.a.b.c".parse().unwrap();
        let mut ctx = json!({});
        p.write(&mut ctx, json!(42)).unwrap();
        assert_eq!(ctx, json!({"a": {"b": {"c": 42}}}));
    }

    #[test]
    fn write_clobber_of_non_object_intermediate_errors_and_leaves_ctx_unchanged() {
        let p: Path = "$.a.b".parse().unwrap();
        let mut ctx = json!({"a": "string"});
        let before = ctx.clone();
        let err = p.write(&mut ctx, json!(1)).unwrap_err();
        assert!(matches!(err, EvalError::TypeError { .. }), "{err:?}");
        assert_eq!(ctx, before, "ctx must be unchanged after a rejected write");
    }

    #[test]
    fn write_top_level_non_object_ctx_errors_and_leaves_ctx_unchanged() {
        let p: Path = "$.a".parse().unwrap();
        let mut ctx = json!("scalar");
        let before = ctx.clone();
        let err = p.write(&mut ctx, json!(1)).unwrap_err();
        assert!(matches!(err, EvalError::TypeError { .. }), "{err:?}");
        assert_eq!(ctx, before);
    }

    #[test]
    fn write_existing_null_intermediate_still_promotes() {
        let p: Path = "$.a.b".parse().unwrap();
        let mut ctx = json!({"a": null});
        p.write(&mut ctx, json!(1)).unwrap();
        assert_eq!(ctx, json!({"a": {"b": 1}}));
    }
}

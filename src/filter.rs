//! RUST_LOG-style per-target level filtering.
//!
//! The [`TargetFilter`] type parses a directive string in the same spirit as
//! `RUST_LOG` and consults it at log time, so callers can tune verbosity per
//! module at runtime through an environment variable instead of being limited
//! to a single global level. No external dependencies are involved.

use crate::level::Level;

/// Environment variable consulted by [`TargetFilter::from_env`] and, unless a
/// filter is supplied explicitly, by [`LoggerBuilder::build`].
///
/// [`LoggerBuilder::build`]: crate::LoggerBuilder::build
pub const DEFAULT_FILTER_ENV: &str = "XOSLOG";

/// A single `target=level` directive parsed from a filter spec.
#[derive(Debug, Clone)]
struct Directive {
    /// Module target the directive applies to; empty for the global default.
    target: String,
    /// Threshold level for that target.
    level: Level,
}

/// A parsed set of per-target level directives.
///
/// The directive grammar mirrors the widely-loved `RUST_LOG` format:
///
/// * `debug` — a bare level sets the default level for every target.
/// * `myapp=debug` — set the threshold for `myapp` and every descendant
///   module (`myapp::*`).
/// * `hyper` — a bare target enables it at the most verbose level (`trace`).
/// * `myapp=` — an empty level is treated like a bare target (most verbose).
/// * `myapp=off` — disable logging for a single target.
///
/// Levels are case-insensitive and `warning` is accepted for `warn`. Directives
/// that cannot be parsed (bad level names, stray `=` signs) are silently
/// skipped, matching `env_logger`'s lenient parsing. A `/regex` suffix is
/// ignored because `xoslog` does not filter on message content.
///
/// When a target matches more than one directive, the longest (most specific)
/// match wins. A directive for `myapp` matches `myapp` and `myapp::foo` but
/// not a sibling such as `myapp_client`. Targets covered by no directive fall
/// back to the global default directive, or to the logger's configured base
/// level when no global directive is present.
///
/// # Example
///
/// ```no_run
/// use xoslog::{Level, LoggerBuilder, TargetFilter};
///
/// let logger = LoggerBuilder::new()
///     .level(Level::Info)
///     .filter(TargetFilter::parse("myapp=debug,hyper=warn"))
///     .build()
///     .unwrap();
/// ```
#[derive(Debug, Clone, Default)]
pub struct TargetFilter {
    directives: Vec<Directive>,
}

impl TargetFilter {
    /// Parse a directive string such as `"myapp=debug,hyper=warn"`.
    ///
    /// Parsing is lenient: malformed directives are skipped, so a partial spec
    /// never fails and never panics. An empty or whitespace-only spec yields an
    /// empty filter that leaves the logger's base level untouched.
    #[must_use]
    pub fn parse(spec: &str) -> TargetFilter {
        let mut directives: Vec<Directive> = Vec::new();
        // `RUST_LOG` allows a `/regex` part after the directives; xoslog has no
        // message filtering, so only the directive list is considered.
        let mods = spec.split('/').next().unwrap_or("");
        for raw in mods.split(',').map(str::trim) {
            if raw.is_empty() {
                continue;
            }
            let mut parts = raw.split('=');
            let (name, level) = match (parts.next(), parts.next().map(str::trim), parts.next()) {
                // A bare token is a level name (global default) or, failing
                // that, a bare target enabled at the most verbose level.
                (Some(part0), None, None) => match Level::parse(part0) {
                    Some(level) => (None, level),
                    None => (Some(part0), Level::Trace),
                },
                // `target=` behaves like a bare target; a bare `=` is garbage
                // and is skipped.
                (Some(part0), Some(""), None) => {
                    if part0.is_empty() {
                        continue;
                    }
                    (Some(part0), Level::Trace)
                }
                (Some(part0), Some(part1), None) => match Level::parse(part1) {
                    Some(level) => (Some(part0), level),
                    None => continue,
                },
                _ => continue,
            };
            let target = name.unwrap_or_default().to_string();
            // A later directive with the same target replaces the earlier one.
            match directives.iter_mut().find(|d| d.target == target) {
                Some(existing) => existing.level = level,
                None => directives.push(Directive { target, level }),
            }
        }
        TargetFilter { directives }
    }

    /// Read the filter from the [`DEFAULT_FILTER_ENV`] environment variable.
    ///
    /// Returns `None` when the variable is unset, empty, or whitespace-only, in
    /// which case the logger's configured base level applies unchanged.
    #[must_use]
    pub fn from_env() -> Option<TargetFilter> {
        TargetFilter::from_env_name(DEFAULT_FILTER_ENV)
    }

    /// Read the filter from a named environment variable.
    #[must_use]
    pub fn from_env_name(name: &str) -> Option<TargetFilter> {
        let spec = std::env::var(name).ok()?;
        if spec.trim().is_empty() {
            return None;
        }
        Some(TargetFilter::parse(&spec))
    }

    /// Whether this filter contains no directives at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.directives.is_empty()
    }

    /// The global default level, if a bare-level directive was given.
    #[must_use]
    pub fn global_default(&self) -> Option<Level> {
        self.directives
            .iter()
            .find(|d| d.target.is_empty())
            .map(|d| d.level)
    }

    /// The default level applied to targets matched by no directive.
    ///
    /// This is the global default directive when present, otherwise `base` —
    /// typically the logger's configured base level.
    #[must_use]
    pub fn default_level(&self, base: Level) -> Level {
        self.global_default().unwrap_or(base)
    }

    /// The effective threshold level for `target`, using `base` as the default.
    ///
    /// The most specific (longest) matching directive wins; a target that
    /// matches no directive gets [`TargetFilter::default_level`].
    #[must_use]
    pub fn effective_level(&self, target: &str, base: Level) -> Level {
        let mut best: Option<(usize, Level)> = None;
        for directive in &self.directives {
            if directive.target.is_empty() {
                continue;
            }
            if module_matches(&directive.target, target) {
                let len = directive.target.len();
                match best {
                    Some((best_len, _)) if best_len > len => {}
                    _ => best = Some((len, directive.level)),
                }
            }
        }
        best.map_or_else(|| self.default_level(base), |(_, level)| level)
    }

    /// Whether a record for `target` at `level` passes the filter, using
    /// `base` as the default level for unmatched targets.
    #[must_use]
    pub fn enabled(&self, target: &str, level: Level, base: Level) -> bool {
        if level == Level::Off {
            return false;
        }
        let effective = self.effective_level(target, base);
        effective != Level::Off && level >= effective
    }
}

/// Whether `directive` matches `target`: an exact hit or a `::`-delimited
/// descendant (`myapp` matches `myapp::server`, not `myapp_client`).
fn module_matches(directive: &str, target: &str) -> bool {
    if target == directive {
        return true;
    }
    target
        .strip_prefix(directive)
        .is_some_and(|rest| rest.starts_with("::"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const INFO: Level = Level::Info;
    const DEBUG: Level = Level::Debug;
    const WARN: Level = Level::Warn;
    const ERROR: Level = Level::Error;
    const TRACE: Level = Level::Trace;
    const OFF: Level = Level::Off;

    #[test]
    fn empty_spec_yields_empty_filter() {
        for spec in ["", "   ", ",", " , ", "   ,"] {
            let filter = TargetFilter::parse(spec);
            assert!(filter.is_empty(), "spec {spec:?} should parse to nothing");
            assert_eq!(filter.global_default(), None);
            // With no directives the base level applies unchanged.
            assert!(filter.enabled("anything", INFO, INFO));
            assert!(!filter.enabled("anything", DEBUG, INFO));
        }
    }

    #[test]
    fn bare_level_is_global_default() {
        for spec in ["debug", "DEBUG", "Debug", "deBuG"] {
            let filter = TargetFilter::parse(spec);
            assert_eq!(filter.global_default(), Some(DEBUG));
            assert_eq!(filter.default_level(INFO), DEBUG);
            assert!(filter.enabled("any::target", DEBUG, INFO));
            assert!(!filter.enabled("any::target", TRACE, INFO));
        }
    }

    #[test]
    fn bare_off_disables_everything() {
        let filter = TargetFilter::parse("off");
        assert_eq!(filter.global_default(), Some(OFF));
        for level in [TRACE, DEBUG, INFO, WARN, ERROR] {
            assert!(!filter.enabled("any::target", level, INFO));
        }
    }

    #[test]
    fn bare_target_enables_at_trace() {
        let filter = TargetFilter::parse("hyper");
        assert_eq!(filter.global_default(), None);
        assert_eq!(filter.effective_level("hyper", INFO), TRACE);
        assert_eq!(filter.effective_level("hyper::client", INFO), TRACE);
        assert!(filter.enabled("hyper::client", TRACE, INFO));
        // Unrelated targets keep the base level.
        assert_eq!(filter.effective_level("myapp", INFO), INFO);
    }

    #[test]
    fn empty_level_is_bare_target() {
        let filter = TargetFilter::parse("hyper=");
        assert_eq!(filter.effective_level("hyper", INFO), TRACE);
    }

    #[test]
    fn target_directive_matches_descendants_not_siblings() {
        let filter = TargetFilter::parse("myapp=debug");
        assert_eq!(filter.effective_level("myapp", INFO), DEBUG);
        assert_eq!(filter.effective_level("myapp::server", INFO), DEBUG);
        assert_eq!(filter.effective_level("myapp::server::tls", INFO), DEBUG);
        // Not a descendant: `myapp_client` must not match `myapp`.
        assert_eq!(filter.effective_level("myapp_client", INFO), INFO);
        assert_eq!(filter.effective_level("myappx", INFO), INFO);
    }

    #[test]
    fn longest_match_wins() {
        let filter = TargetFilter::parse("crate2=info,crate2::mod=debug");
        assert_eq!(filter.effective_level("crate2", INFO), INFO);
        assert_eq!(filter.effective_level("crate2::mod", INFO), DEBUG);
        assert_eq!(filter.effective_level("crate2::mod::deep", INFO), DEBUG);
    }

    #[test]
    fn target_off_suppresses_only_that_target() {
        let filter = TargetFilter::parse("myapp=off");
        for level in [TRACE, DEBUG, INFO, WARN, ERROR] {
            assert!(!filter.enabled("myapp::x", level, INFO), "level {level:?}");
        }
        assert!(filter.enabled("other", INFO, INFO));
        assert!(filter.enabled("other", ERROR, INFO));
    }

    #[test]
    fn mixed_spec_example() {
        let filter = TargetFilter::parse("myapp=debug,hyper=warn");
        assert!(filter.enabled("myapp", DEBUG, INFO));
        assert!(filter.enabled("myapp::sub", INFO, INFO));
        assert!(filter.enabled("hyper", WARN, INFO));
        assert!(!filter.enabled("hyper", INFO, INFO));
        // Unmatched targets fall back to the base level.
        assert!(filter.enabled("unrelated", INFO, INFO));
        assert!(!filter.enabled("unrelated", DEBUG, INFO));
    }

    #[test]
    fn global_level_with_overrides() {
        let filter = TargetFilter::parse("debug,hyper=warn");
        assert_eq!(filter.default_level(INFO), DEBUG);
        assert!(filter.enabled("myapp", DEBUG, INFO));
        assert!(!filter.enabled("myapp", TRACE, INFO));
        // The explicit target directive overrides the global default.
        assert!(!filter.enabled("hyper", DEBUG, INFO));
        assert!(filter.enabled("hyper", WARN, INFO));
    }

    #[test]
    fn invalid_directives_are_skipped() {
        // Unknown level, too many '=', trailing '=' with extra part.
        let filter = TargetFilter::parse("foo=invalid,bad=a=b,ok=warn,=,");
        assert_eq!(filter.global_default(), None);
        assert_eq!(filter.effective_level("ok", INFO), WARN);
        // foo/bad never become directives.
        assert_eq!(filter.effective_level("foo", INFO), INFO);
        assert_eq!(filter.effective_level("bad", INFO), INFO);
    }

    #[test]
    fn later_directive_replaces_earlier() {
        let filter = TargetFilter::parse("myapp=info,myapp=debug");
        assert_eq!(filter.effective_level("myapp", INFO), DEBUG);
    }

    #[test]
    fn warning_alias_and_case_insensitive_levels() {
        let filter = TargetFilter::parse("a=WARNING,b=WaRn,c=Off");
        assert_eq!(filter.effective_level("a", INFO), WARN);
        assert_eq!(filter.effective_level("b", INFO), WARN);
        assert_eq!(filter.effective_level("c", INFO), OFF);
        assert!(!filter.enabled("c", ERROR, INFO));
    }

    #[test]
    fn regex_suffix_is_ignored() {
        let filter = TargetFilter::parse("myapp=debug/secret-pattern");
        assert_eq!(filter.effective_level("myapp", INFO), DEBUG);
    }

    #[test]
    fn off_level_never_enabled() {
        let filter = TargetFilter::parse("myapp=debug");
        assert!(!filter.enabled("myapp", OFF, INFO));
    }

    #[test]
    fn base_off_disables_unmatched() {
        let filter = TargetFilter::parse("myapp=debug");
        assert!(filter.enabled("myapp", DEBUG, OFF));
        assert!(!filter.enabled("other", INFO, OFF));
    }

    #[test]
    fn module_matches_boundary() {
        assert!(module_matches("myapp", "myapp"));
        assert!(module_matches("myapp", "myapp::server"));
        assert!(!module_matches("myapp", "myapp_client"));
        assert!(!module_matches("myapp", "my"));
        assert!(!module_matches("", "myapp"));
    }
}

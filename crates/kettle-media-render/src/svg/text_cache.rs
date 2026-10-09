//! Per-job reuse of native shaping facts; every hit still observes cancellation.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use kettle_media::FailureCode;

use super::fonts::JobFonts;
use super::text_metrics::{self, FontSlant, Metrics, TextShape, TextStyle, TextWhitespace};

type Value = Arc<Option<Metrics>>;
type Labels = HashMap<String, Value>;
type Styles = HashMap<StyleKey, Labels>;

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
struct StyleKey {
    size: u64,
    weight: u16,
    slant: FontSlant,
    shape: TextShape,
    whitespace: TextWhitespace,
}

#[derive(Clone, Copy)]
struct Limits {
    requests: usize,
    misses: usize,
    work_bytes: usize,
    shaped_bytes: usize,
}

const LIMITS: Limits = Limits {
    requests: 4096,
    misses: 512,
    work_bytes: 4 * 1024 * 1024,
    shaped_bytes: 1024 * 1024,
};

#[derive(Default)]
struct State {
    families: HashMap<String, Styles>,
    requests: usize,
    misses: usize,
    work_bytes: usize,
    shaped_bytes: usize,
}

pub(crate) struct NativeMeasurements {
    fonts: Arc<JobFonts>,
    state: Mutex<State>,
    limits: Limits,
}

impl NativeMeasurements {
    pub(crate) fn new(fonts: Arc<JobFonts>) -> Self {
        Self {
            fonts,
            state: Mutex::new(State::default()),
            limits: LIMITS,
        }
    }

    /// The caller supplies its original operation checkpoint, including its
    /// absolute deadline. Cache hits never extend that operation's lifetime.
    #[cfg(test)]
    pub(crate) fn measure(
        &self,
        text: &str,
        style: &TextStyle<'_>,
        checkpoint: impl Fn() -> Result<(), FailureCode>,
    ) -> Result<Value, FailureCode> {
        self.measure_shape(text, style, TextShape::Raw, checkpoint)
    }

    #[cfg(test)]
    pub(crate) fn measure_shape(
        &self,
        text: &str,
        style: &TextStyle<'_>,
        shape: TextShape,
        checkpoint: impl Fn() -> Result<(), FailureCode>,
    ) -> Result<Value, FailureCode> {
        self.measure_shape_with_whitespace(text, style, shape, TextWhitespace::Preserve, checkpoint)
    }

    pub(crate) fn measure_shape_with_whitespace(
        &self,
        text: &str,
        style: &TextStyle<'_>,
        shape: TextShape,
        whitespace: TextWhitespace,
        checkpoint: impl Fn() -> Result<(), FailureCode>,
    ) -> Result<Value, FailureCode> {
        checkpoint()?;
        let weight = text_metrics::validate(text, style)?;
        let key = StyleKey {
            size: style.size.to_bits(),
            weight,
            slant: style.slant,
            shape,
            whitespace,
        };
        let bytes = text
            .len()
            .checked_add(style.family.len())
            .ok_or(FailureCode::RenderResource)?;
        let cached = {
            let mut state = self.state.lock().map_err(|_| FailureCode::RenderResource)?;
            let requests = state
                .requests
                .checked_add(1)
                .ok_or(FailureCode::RenderResource)?;
            let work_bytes = state
                .work_bytes
                .checked_add(bytes)
                .ok_or(FailureCode::RenderResource)?;
            if requests > self.limits.requests || work_bytes > self.limits.work_bytes {
                return Err(FailureCode::RenderResource);
            }
            state.requests = requests;
            state.work_bytes = work_bytes;
            let cached = state
                .families
                .get(style.family)
                .and_then(|styles| styles.get(&key))
                .and_then(|labels| labels.get(text))
                .cloned();
            if cached.is_none() {
                let misses = state
                    .misses
                    .checked_add(1)
                    .ok_or(FailureCode::RenderResource)?;
                let shaped_bytes = state
                    .shaped_bytes
                    .checked_add(bytes)
                    .ok_or(FailureCode::RenderResource)?;
                if misses > self.limits.misses || shaped_bytes > self.limits.shaped_bytes {
                    return Err(FailureCode::RenderResource);
                }
                // Reserve work before dropping the mutex, including concurrent
                // duplicates. Never hold it while shaping or calling the host.
                state.misses = misses;
                state.shaped_bytes = shaped_bytes;
            }
            cached
        };
        if let Some(cached) = cached {
            checkpoint()?;
            return Ok(cached);
        }
        let measured = Arc::new(text_metrics::measure_shape_with_whitespace(
            text,
            style,
            shape,
            whitespace,
            &self.fonts,
        )?);
        checkpoint()?;
        let mut state = self.state.lock().map_err(|_| FailureCode::RenderResource)?;
        Ok(state
            .families
            .entry(style.family.to_owned())
            .or_default()
            .entry(key)
            .or_default()
            .entry(text.to_owned())
            .or_insert(measured)
            .clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> NativeMeasurements {
        NativeMeasurements::new(Arc::new(super::super::fonts::for_mermaid_job(&[]).unwrap()))
    }

    fn style(cache: &NativeMeasurements) -> TextStyle<'_> {
        TextStyle {
            family: &cache.fonts.family,
            size: 24.0,
            weight: "normal",
            slant: FontSlant::Normal,
        }
    }

    #[test]
    fn cache_does_not_reuse_literal_space_facts_for_svg_default_whitespace() {
        let cache = fixture();
        let selected = style(&cache);
        let text = "  A  B  ";
        let preserve = cache.measure(text, &selected, || Ok(())).unwrap();
        let default = cache
            .measure_shape_with_whitespace(
                text,
                &selected,
                TextShape::Raw,
                TextWhitespace::SvgDefault,
                || Ok(()),
            )
            .unwrap();
        assert!(!Arc::ptr_eq(&preserve, &default));
        assert!(
            preserve.as_ref().as_ref().unwrap().advance
                > default.as_ref().as_ref().unwrap().advance
        );
        let again = cache
            .measure_shape_with_whitespace(
                text,
                &selected,
                TextShape::Raw,
                TextWhitespace::SvgDefault,
                || Ok(()),
            )
            .unwrap();
        assert!(Arc::ptr_eq(&default, &again));
        let state = cache.state.lock().unwrap();
        assert_eq!((state.requests, state.misses), (3, 2));
    }

    #[test]
    fn cache_does_not_reuse_facts_across_dom_shapes() {
        let cache = fixture();
        let selected = style(&cache);
        let raw = cache
            .measure_shape("Av", &selected, TextShape::Raw, || Ok(()))
            .unwrap();
        let formatted = cache
            .measure_shape("Av", &selected, TextShape::Formatted, || Ok(()))
            .unwrap();
        assert!(!Arc::ptr_eq(&raw, &formatted));
        assert_ne!(
            raw.as_ref().as_ref().unwrap().bounds.y(),
            formatted.as_ref().as_ref().unwrap().bounds.y()
        );
        let again = cache
            .measure_shape("Av", &selected, TextShape::Formatted, || Ok(()))
            .unwrap();
        assert!(Arc::ptr_eq(&formatted, &again));
    }

    #[test]
    fn repeated_labels_share_native_metrics_without_reshaping() {
        let cache = fixture();
        let a = cache.measure("AV ffi", &style(&cache), || Ok(())).unwrap();
        let b = cache.measure("AV ffi", &style(&cache), || Ok(())).unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        let state = cache.state.lock().unwrap();
        assert_eq!((state.requests, state.misses), (2, 1));
        assert!(a.as_ref().as_ref().unwrap().advance > 0.0);
    }

    #[test]
    fn styles_have_independent_metrics_but_equivalent_weight_aliases_reuse() {
        let cache = fixture();
        let mut style = style(&cache);
        let regular = cache.measure("AV ffi", &style, || Ok(())).unwrap();
        style.weight = "400";
        let alias = cache.measure("AV ffi", &style, || Ok(())).unwrap();
        assert!(Arc::ptr_eq(&regular, &alias));
        style.weight = "bold";
        let bold = cache.measure("AV ffi", &style, || Ok(())).unwrap();
        assert!(!Arc::ptr_eq(&regular, &bold));
        style.slant = FontSlant::Italic;
        let italic = cache.measure("AV ffi", &style, || Ok(())).unwrap();
        assert!(!Arc::ptr_eq(&bold, &italic));
        style.size = 48.0;
        let large = cache.measure("AV ffi", &style, || Ok(())).unwrap();
        let advance = |v: &Value| v.as_ref().as_ref().unwrap().advance;
        assert!((advance(&large) - 2.0 * advance(&italic)).abs() < 0.001);
    }

    #[test]
    fn italic_and_oblique_keep_distinct_native_cache_identity() {
        let cache = fixture();
        let mut selected = style(&cache);
        selected.slant = FontSlant::Italic;
        let italic = cache.measure("AV ffi", &selected, || Ok(())).unwrap();
        selected.slant = FontSlant::Oblique;
        let oblique = cache.measure("AV ffi", &selected, || Ok(())).unwrap();
        assert!(!Arc::ptr_eq(&italic, &oblique));
        let again = cache.measure("AV ffi", &selected, || Ok(())).unwrap();
        assert!(Arc::ptr_eq(&oblique, &again));
        assert_eq!(cache.state.lock().unwrap().misses, 2);
    }

    #[test]
    fn cached_metrics_never_cross_job_font_databases() {
        let proportional = fixture();
        let mono = NativeMeasurements::new(Arc::new(super::super::fonts::for_job(&[]).unwrap()));
        let a = proportional
            .measure("iiii", &style(&proportional), || Ok(()))
            .unwrap();
        let b = mono.measure("iiii", &style(&mono), || Ok(())).unwrap();
        assert!(!Arc::ptr_eq(&a, &b));
        assert_ne!(
            a.as_ref().as_ref().unwrap().advance,
            b.as_ref().as_ref().unwrap().advance
        );
    }

    #[test]
    fn empty_measurements_are_cached_as_empty_instead_of_a_fake_box() {
        let cache = fixture();
        let a = cache.measure("", &style(&cache), || Ok(())).unwrap();
        let b = cache.measure("", &style(&cache), || Ok(())).unwrap();
        assert!(a.is_none() && Arc::ptr_eq(&a, &b));
        assert_eq!(cache.state.lock().unwrap().misses, 1);
    }

    #[test]
    fn invalid_styles_do_not_consume_a_valid_jobs_cache_budget() {
        let cache = fixture();
        let mut invalid = style(&cache);
        invalid.size = f64::NAN;
        assert!(matches!(
            cache.measure("label", &invalid, || Ok(())),
            Err(FailureCode::RenderParse)
        ));
        assert_eq!(cache.state.lock().unwrap().requests, 0);
        assert!(cache.measure("label", &style(&cache), || Ok(())).is_ok());
    }

    #[test]
    fn hits_observe_cancellation_and_the_original_request_budget() {
        let mut cache = fixture();
        cache.limits.requests = 2;
        cache.measure("label", &style(&cache), || Ok(())).unwrap();
        assert!(matches!(
            cache.measure("label", &style(&cache), || Err(FailureCode::RenderTimeout)),
            Err(FailureCode::RenderTimeout)
        ));
        assert_eq!(cache.state.lock().unwrap().requests, 1);
        cache.measure("label", &style(&cache), || Ok(())).unwrap();
        assert!(matches!(
            cache.measure("label", &style(&cache), || Ok(())),
            Err(FailureCode::RenderResource)
        ));
        assert_eq!(cache.state.lock().unwrap().misses, 1);
    }

    #[test]
    fn cold_source_work_is_bounded_before_any_native_probe() {
        let mut cache = fixture();
        cache.limits.shaped_bytes = 1;
        assert!(matches!(
            cache.measure("label", &style(&cache), || Ok(())),
            Err(FailureCode::RenderResource)
        ));
        let state = cache.state.lock().unwrap();
        assert_eq!((state.misses, state.shaped_bytes), (0, 0));
        assert!(state.families.is_empty());
    }

    #[test]
    fn a_deadline_after_shaping_prevents_cache_publication() {
        let cache = fixture();
        let calls = std::cell::Cell::new(0);
        let result = cache.measure("label", &style(&cache), || {
            calls.set(calls.get() + 1);
            if calls.get() == 2 {
                Err(FailureCode::RenderTimeout)
            } else {
                Ok(())
            }
        });
        assert!(matches!(result, Err(FailureCode::RenderTimeout)));
        let state = cache.state.lock().unwrap();
        assert_eq!(state.misses, 1);
        assert!(state.families.is_empty());
    }
}

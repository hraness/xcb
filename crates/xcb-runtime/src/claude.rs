use crate::{Error, Result};
use serde_json::Value;
use xcb_core::{
    MAX_JSON_BYTES, MAX_TEXT_BYTES, display_text,
    policy::{Failure, Terminal},
    usage::Counters,
};

/// Oldest admitted installed Claude Code release within major 2. The
/// qualification record still binds the exact inspected version and
/// executable SHA-256, and the init assertion re-proves the effective
/// boundary on every run; the floor only decides which binaries doctor may
/// admit so routine CLI patch/minor releases stop revoking native execution.
pub const MIN_VERSION: &str = "2.1.268";
pub const MAX_MAJOR: u64 = 2;

fn version_tuple(version: &str) -> Option<[u64; 3]> {
    let parts: Vec<&str> = version.split('.').collect();
    if parts.len() != 3
        || parts.iter().any(|part| {
            part.is_empty() || part.len() > 9 || !part.bytes().all(|b| b.is_ascii_digit())
        })
    {
        return None;
    }
    let mut tuple = [0u64; 3];
    for (index, part) in parts.iter().enumerate() {
        tuple[index] = part.parse().ok()?;
    }
    Some(tuple)
}

pub fn version_admitted(version: &str) -> bool {
    let Some(got) = version_tuple(version) else {
        return false;
    };
    let Some(min) = version_tuple(MIN_VERSION) else {
        return false;
    };
    if got[0] != MAX_MAJOR {
        return false;
    }
    got[1] > min[1] || (got[1] == min[1] && got[2] >= min[2])
}

#[derive(Debug, Clone, PartialEq)]
pub struct QuotaObservation {
    pub window: String,
    pub utilization: f64,
    pub resets_at_ms: Option<u64>,
}

const QUOTA_WINDOWS: [&str; 6] = [
    "five_hour",
    "seven_day",
    "seven_day_opus",
    "seven_day_sonnet",
    "seven_day_overage_included",
    "overage",
];

fn quota_window(value: Option<&str>) -> Option<String> {
    value
        .filter(|value| QUOTA_WINDOWS.contains(value))
        .map(str::to_owned)
}

fn quota_utilization(value: Option<&Value>) -> Option<f64> {
    value
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
        .map(|value| value.clamp(0.0, 1.0))
}

fn quota_reset(value: Option<&Value>) -> Option<u64> {
    value
        .and_then(Value::as_u64)
        .filter(|value| *value < 100_000_000_000)
        .and_then(|value| value.checked_mul(1000))
}

#[derive(Debug)]
pub enum Event {
    Initialize(Value),
    Control(Value),
    ControlResponse(Value),
    Delta {
        thinking: bool,
        text: String,
    },
    Assistant {
        text: String,
    },
    Quota {
        /// One meter per reported window. Claude Code 2.1.282 reports every
        /// window in `unifiedWindows`; older builds report one at the top.
        observations: Vec<QuotaObservation>,
        failure: Option<Failure>,
        /// Telemetry drift the host reports without failing the turn.
        notice: Option<&'static str>,
    },
    Result {
        terminal: Terminal,
        text: String,
        models: Vec<(String, Counters)>,
        /// Account-level classification of a provider-marked error result.
        failure: Option<Failure>,
    },
    Subagent {
        id: String,
        status: String,
        label: String,
        model: Option<String>,
    },
    Notice,
}
fn string<'a>(value: &'a Value, key: &str, max: usize) -> Result<&'a str> {
    crate::wire_helpers::field_text(value, key, max, "missing string", "oversized string")
}
fn optional_string(value: &Value, key: &str, max: usize) -> Result<Option<String>> {
    value
        .get(key)
        .filter(|value| !value.is_null())
        .map(|_| string(value, key, max).map(str::to_owned))
        .transpose()
}
fn number(value: &Value, key: &str) -> Result<u64> {
    crate::wire_helpers::field_counter(value, key, "missing counter")
}
fn maybe_count(value: &Value, key: &str) -> Result<u64> {
    crate::wire_helpers::optional_field_counter(value, key, "invalid counter")
}
fn counters(value: &Value) -> Result<Counters> {
    let value = Counters {
        input: number(value, "inputTokens")?,
        output: number(value, "outputTokens")?,
        cache_read: maybe_count(value, "cacheReadInputTokens")?,
        cache_write: maybe_count(value, "cacheCreationInputTokens")?,
        reasoning: value
            .get("thinkingTokens")
            .filter(|value| !value.is_null())
            .map(|value| {
                value
                    .as_u64()
                    .ok_or(Error::Protocol("invalid thinking counter"))
            })
            .transpose()?,
    };
    value.total()?;
    Ok(value)
}

/// Conservative match on the pinned CLI's authentication error text. It is
/// consulted only for frames the provider itself marked as errors, so it can
/// classify a failure but never fail a turn on its own.
pub(crate) fn authentication_cue(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        "not logged in",
        "/login",
        "invalid api key",
        "authentication_error",
        "authentication failed",
        "invalid authentication",
        "unauthorized",
        "oauth token",
        "expired token",
        "token has expired",
        "token expired",
        "invalid token",
    ]
    .iter()
    .any(|cue| lower.contains(cue))
}

pub fn parse_event(bytes: &[u8]) -> Result<Event> {
    if bytes.len() > MAX_JSON_BYTES {
        return Err(Error::Protocol("frame limit"));
    }
    parse_value(serde_json::from_slice(bytes)?)
}

/// Classify one already-parsed frame whose byte length was bounded by the
/// reader; callers that need other fields of the same frame parse it once.
pub fn parse_value(value: Value) -> Result<Event> {
    match string(&value, "type", 80)? {
        "control_request" => Ok(Event::Control(value)),
        "control_response" => Ok(Event::ControlResponse(value)),
        "system" => match value.get("subtype").and_then(Value::as_str) {
            Some("init") => Ok(Event::Initialize(value)),
            Some("task_started") => Ok(Event::Subagent {
                id: string(&value, "task_id", 160)?.to_owned(),
                status: "working".into(),
                label: display_text(
                    value
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or("Subagent"),
                    160,
                ),
                model: optional_string(&value, "model", 160)?,
            }),
            Some("task_notification") => Ok(Event::Subagent {
                id: string(&value, "task_id", 160)?.to_owned(),
                status: string(&value, "status", 40)?.to_owned(),
                label: "Subagent".into(),
                model: optional_string(&value, "model", 160)?,
            }),
            _ => Ok(Event::Notice),
        },
        "stream_event" => {
            let event = value
                .get("event")
                .ok_or(Error::Protocol("missing stream event"))?;
            if event.get("type").and_then(Value::as_str) != Some("content_block_delta") {
                return Ok(Event::Notice);
            }
            let delta = event.get("delta").ok_or(Error::Protocol("missing delta"))?;
            match delta.get("type").and_then(Value::as_str) {
                Some("text_delta") => Ok(Event::Delta {
                    thinking: false,
                    text: display_text(string(delta, "text", MAX_TEXT_BYTES)?, MAX_TEXT_BYTES),
                }),
                Some("thinking_delta") => Ok(Event::Delta {
                    thinking: true,
                    text: display_text(string(delta, "thinking", MAX_TEXT_BYTES)?, MAX_TEXT_BYTES),
                }),
                _ => Ok(Event::Notice),
            }
        }
        "assistant" => {
            let blocks = value
                .pointer("/message/content")
                .and_then(Value::as_array)
                .ok_or(Error::Protocol("assistant content"))?;
            if blocks.len() > 128 {
                return Err(Error::Protocol("content block limit"));
            }
            let mut text = String::new();
            let mut thinking = String::new();
            for block in blocks {
                match block.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if !text.is_empty() {
                            text.push('\n');
                        }
                        text.push_str(string(block, "text", MAX_TEXT_BYTES)?);
                    }
                    Some("thinking") => {
                        if !thinking.is_empty() {
                            thinking.push('\n');
                        }
                        thinking.push_str(string(block, "thinking", MAX_TEXT_BYTES)?);
                    }
                    _ => (),
                }
                if text.len() + thinking.len() > MAX_TEXT_BYTES {
                    return Err(Error::Protocol("assistant text limit"));
                }
            }
            // Thinking blocks are bounded above but never retained here: the
            // host renders streamed thinking deltas, not the final message copy.
            Ok(Event::Assistant {
                text: display_text(&text, MAX_TEXT_BYTES),
            })
        }
        "rate_limit_event" => {
            // Telemetry only. Unknown windows, statuses and out-of-range
            // meters are reported and tolerated; only an explicit rejection
            // can fail the turn, and it does so even with an unknown window.
            let info = value
                .get("rate_limit_info")
                .ok_or(Error::Protocol("rate limit info"))?;
            let (rejected, notice) = match info.get("status").and_then(Value::as_str) {
                Some("rejected") => (true, None),
                Some("allowed" | "allowed_warning") => (false, None),
                _ => (
                    false,
                    Some("Claude reported an unrecognized rate limit status; treated as allowed"),
                ),
            };
            let utilization = quota_utilization(info.get("utilization"));
            let resets_at_ms = quota_reset(info.get("resetsAt"));
            let window = quota_window(info.get("rateLimitType").and_then(Value::as_str));
            // Claude Code 2.1.282 reports every window's meter under
            // `unifiedWindows` and no longer sets the top-level utilization;
            // older builds report the current window at the top. Read both,
            // bounded, and keep one observation per recognized window.
            let mut observations: Vec<QuotaObservation> = Vec::new();
            if let Some(unified) = info.get("unifiedWindows").and_then(Value::as_object) {
                if unified.len() > 16 {
                    return Err(Error::Protocol("rate limit window limit"));
                }
                for (name, meter) in unified {
                    if let (Some(window), Some(utilization)) = (
                        quota_window(Some(name.as_str())),
                        quota_utilization(meter.get("utilization")),
                    ) {
                        observations.push(QuotaObservation {
                            window,
                            utilization,
                            resets_at_ms: quota_reset(meter.get("resetsAt")),
                        });
                    }
                }
            }
            if let (Some(window), Some(utilization)) = (window.clone(), utilization)
                && !observations.iter().any(|seen| seen.window == window)
            {
                observations.push(QuotaObservation {
                    window,
                    utilization,
                    resets_at_ms,
                });
            }
            // A rejection is exhaustion of its window even when no meter is
            // reported for it; the reset, when known, bounds the block.
            if rejected
                && let Some(window) = window.clone()
                && !observations.iter().any(|seen| seen.window == window)
            {
                observations.push(QuotaObservation {
                    window,
                    utilization: 1.0,
                    resets_at_ms,
                });
            }
            let failure = rejected.then_some(
                if matches!(
                    window.as_deref(),
                    Some("seven_day_opus" | "seven_day_sonnet")
                ) {
                    Failure::ModelQuota
                } else {
                    Failure::AccountQuota
                },
            );
            Ok(Event::Quota {
                observations,
                failure,
                notice,
            })
        }
        "result" => {
            let subtype = string(&value, "subtype", 80)?;
            let is_error = value
                .get("is_error")
                .and_then(Value::as_bool)
                .ok_or(Error::Protocol("result status"))?;
            let terminal = match (
                subtype,
                is_error,
                value.get("stop_reason").and_then(Value::as_str),
            ) {
                ("error_max_turns", _, _) => Terminal::TurnLimit,
                ("success", false, Some("max_tokens")) => Terminal::TokenLimit,
                ("success", false, _) => Terminal::Completed,
                _ => Terminal::Failed,
            };
            let mut text = value
                .get("result")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            if let Some(errors) = value.get("errors") {
                let errors = errors.as_array().ok_or(Error::Protocol("result errors"))?;
                if errors.len() > 32 {
                    return Err(Error::Protocol("result error limit"));
                }
                for error in errors {
                    let error = error.as_str().ok_or(Error::Protocol("result error text"))?;
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(error);
                }
            }
            if text.len() > MAX_TEXT_BYTES {
                return Err(Error::Protocol("result text limit"));
            }
            let mut models = Vec::new();
            if let Some(raw) = value.get("modelUsage") {
                let raw = raw.as_object().ok_or(Error::Protocol("model accounting"))?;
                if raw.len() > 32 {
                    return Err(Error::Protocol("model accounting limit"));
                }
                for (id, value) in raw {
                    xcb_core::Id::new(id)?;
                    models.push((id.to_owned(), counters(value)?));
                }
            }
            let failure = (is_error && terminal == Terminal::Failed && authentication_cue(&text))
                .then_some(Failure::Authentication);
            Ok(Event::Result {
                terminal,
                text: display_text(&text, MAX_TEXT_BYTES),
                models,
                failure,
            })
        }
        _ => Ok(Event::Notice),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn quota(value: Value) -> (Vec<QuotaObservation>, Option<Failure>, Option<&'static str>) {
        match parse_event(&serde_json::to_vec(&value).unwrap()).unwrap() {
            Event::Quota {
                observations,
                failure,
                notice,
            } => (observations, failure, notice),
            _ => panic!("rate_limit_event must stay a quota observation"),
        }
    }

    fn meters(observations: &[QuotaObservation]) -> Vec<(&str, f64, Option<u64>)> {
        observations
            .iter()
            .map(|o| (o.window.as_str(), o.utilization, o.resets_at_ms))
            .collect()
    }

    #[test]
    fn rate_limit_telemetry_tolerates_drift_but_rejection_still_classifies() {
        let frame = |info: Value| json!({"type":"rate_limit_event","rate_limit_info":info});
        // Unknown windows, statuses and out-of-range meters are drift the host
        // reports without failing a turn that may already have run tools.
        let (observations, failure, notice) = quota(frame(json!({
            "status":"throttled","rateLimitType":"five_hour","utilization":0.5
        })));
        assert_eq!(meters(&observations), [("five_hour", 0.5, None)]);
        assert_eq!(failure, None);
        assert!(notice.is_some());
        let (observations, failure, _) = quota(frame(json!({
            "status":"allowed","rateLimitType":"monthly_enterprise","utilization":1.7
        })));
        assert!(observations.is_empty());
        assert_eq!(failure, None);
        let (observations, _, _) = quota(frame(json!({
            "status":"allowed_warning","rateLimitType":"seven_day","utilization":-0.25
        })));
        assert_eq!(meters(&observations), [("seven_day", 0.0, None)]);
        let (observations, _, _) = quota(frame(json!({
            "status":"allowed","rateLimitType":"seven_day","utilization":"high"
        })));
        assert!(observations.is_empty());
        // An explicit rejection classifies even when its window is unknown,
        // and a recognized rejected window records its exhaustion.
        for (window, expected, recorded) in [
            (json!("quarterly"), Failure::AccountQuota, None),
            (Value::Null, Failure::AccountQuota, None),
            (
                json!("seven_day_opus"),
                Failure::ModelQuota,
                Some("seven_day_opus"),
            ),
            (json!("five_hour"), Failure::AccountQuota, Some("five_hour")),
        ] {
            let (observations, failure, _) = quota(frame(json!({
                "status":"rejected","rateLimitType":window,"utilization":1.0
            })));
            assert_eq!(failure, Some(expected));
            assert_eq!(
                meters(&observations),
                recorded.map(|w| vec![(w, 1.0, None)]).unwrap_or_default()
            );
        }
        // A missing rate_limit_info object is still malformed, not drift.
        assert!(parse_event(br#"{"type":"rate_limit_event"}"#).is_err());
    }

    /// Claude Code 2.1.282 reports every window under `unifiedWindows` and no
    /// top-level meter. Every recognized window is observed, unknown ones are
    /// ignored, and a rejection without a meter for its window is recorded as
    /// exhaustion with the frame's reset.
    #[test]
    fn unified_windows_report_every_meter_and_a_rejection_records_exhaustion() {
        let frame = |info: Value| json!({"type":"rate_limit_event","rate_limit_info":info});
        let (observations, failure, notice) = quota(frame(json!({
            "status":"allowed","resetsAt":1790308800,"rateLimitType":"five_hour",
            "overageStatus":"rejected","overageDisabledReason":"org_level_disabled","isUsingOverage":false,
            "unifiedWindows":{
                "five_hour":{"utilization":0.13,"resetsAt":1790308800},
                "seven_day":{"utilization":0.03,"resetsAt":1790722800},
                "quarterly":{"utilization":0.9,"resetsAt":1790722800},
                "seven_day_opus":{"utilization":"n/a"}
            }
        })));
        assert_eq!(
            meters(&observations),
            [
                ("five_hour", 0.13, Some(1_790_308_800_000)),
                ("seven_day", 0.03, Some(1_790_722_800_000)),
            ]
        );
        assert_eq!((failure, notice), (None, None));
        // The top-level meter is used only when the window is not unified.
        let (observations, _, _) = quota(frame(json!({
            "status":"allowed","rateLimitType":"five_hour","utilization":0.4,"resetsAt":1790308800,
            "unifiedWindows":{"seven_day":{"utilization":0.5,"resetsAt":1790722800}}
        })));
        assert_eq!(
            meters(&observations),
            [
                ("seven_day", 0.5, Some(1_790_722_800_000)),
                ("five_hour", 0.4, Some(1_790_308_800_000)),
            ]
        );
        // Rejection: the window's meter is missing, so it is recorded as exhausted.
        let (observations, failure, _) = quota(frame(json!({
            "status":"rejected","rateLimitType":"five_hour","resetsAt":1790308800,
            "unifiedWindows":{"seven_day":{"utilization":0.03,"resetsAt":1790722800}}
        })));
        assert_eq!(failure, Some(Failure::AccountQuota));
        assert_eq!(
            meters(&observations),
            [
                ("seven_day", 0.03, Some(1_790_722_800_000)),
                ("five_hour", 1.0, Some(1_790_308_800_000)),
            ]
        );
        // Too many windows is malformed, not drift.
        let many: serde_json::Map<String, Value> = (0..17)
            .map(|i| (format!("w{i}"), json!({"utilization":0.1})))
            .collect();
        assert!(
            parse_event(
                &serde_json::to_vec(&frame(json!({"status":"allowed","unifiedWindows":many})))
                    .unwrap()
            )
            .is_err()
        );
    }

    #[test]
    fn provider_error_results_classify_authentication_conservatively() {
        let failure =
            |value: Value| match parse_event(&serde_json::to_vec(&value).unwrap()).unwrap() {
                Event::Result {
                    terminal, failure, ..
                } => (terminal, failure),
                _ => panic!("result frame must stay a result"),
            };
        for text in [
            "Not logged in · Please run /login",
            "OAuth token has expired",
            "401 Unauthorized: invalid authentication",
        ] {
            let (terminal, classified) = failure(json!({
                "type":"result","subtype":"error_during_execution","is_error":true,
                "result":format!("Synthetic provider text: {text}"),
            }));
            assert_eq!(
                (terminal, classified),
                (Terminal::Failed, Some(Failure::Authentication)),
                "{text}"
            );
        }
        let (terminal, classified) = failure(json!({
            "type":"result","subtype":"error_during_execution","is_error":true,
            "errors":["invalid api key provided"],
        }));
        assert_eq!(
            (terminal, classified),
            (Terminal::Failed, Some(Failure::Authentication))
        );
        // Non-auth provider text stays unknown, and a turn limit or a
        // successful answer is never reclassified as authentication.
        assert_eq!(
            failure(json!({
                "type":"result","subtype":"error_during_execution","is_error":true,
                "result":"synthetic renderer crash",
            })),
            (Terminal::Failed, None)
        );
        assert_eq!(
            failure(json!({
                "type":"result","subtype":"error_max_turns","is_error":true,
                "result":"please log in to continue",
            })),
            (Terminal::TurnLimit, None)
        );
        assert_eq!(
            failure(json!({
                "type":"result","subtype":"success","is_error":false,
                "result":"I could not log in to the deployment target",
            })),
            (Terminal::Completed, None)
        );
    }
}

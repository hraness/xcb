use crate::{
    Error, Result,
    config::JudgeConfig,
    judge::{self, Judge, JudgeAnswer, JudgeAnswers, JudgeQuestions},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::Value;
use std::{io::Cursor, sync::Arc, time::Duration};
use xcb_core::Id;
use zeroize::Zeroizing;

pub const DEFAULT_MODEL: &str = "clef";
pub const ACCOUNT_ENV: &str = "CLOUDFLARE_ACCOUNT_ID";
pub const TOKEN_ENV: &str = "CLOUDFLARE_API_TOKEN";
pub const TOKEN_ALIAS_ENV: &str = "CLOUDFLARE_AUTH_TOKEN";
pub const MODEL_ENV: &str = "XCB_CLEF_MODEL";
pub const MAX_IMAGE_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_TOTAL_IMAGE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_BODY_BYTES: usize = 13 * 1024 * 1024;
pub const MAX_PIXELS: u64 = 16_000_000;

pub fn valid_account(account: &str) -> bool {
    account.len() == 32 && account.bytes().all(|c| c.is_ascii_hexdigit())
}
pub fn endpoint(account: &str, model: &str) -> Result<String> {
    if !valid_account(account) || !matches!(model, "clef" | "clef-flash") {
        return Err(xcb_core::Error::Invalid("Cloudflare Clef account or model").into());
    }
    Ok(format!(
        "https://api.cloudflare.com/client/v4/accounts/{account}/ai/run/@cf/cloudflare/{model}"
    ))
}

pub fn target(
    config: &JudgeConfig,
    account_env: Option<String>,
    model_env: Option<String>,
) -> Result<(Id, Option<String>)> {
    let model = config
        .model
        .clone()
        .map(String::from)
        .or(model_env)
        .unwrap_or_else(|| DEFAULT_MODEL.into());
    if !matches!(model.as_str(), "clef" | "clef-flash") {
        return Err(xcb_core::Error::Invalid("Clef model").into());
    }
    let account = account_env;
    let url = account.map(|id| endpoint(&id, &model)).transpose()?;
    if config
        .endpoint
        .as_ref()
        .is_some_and(|configured| Some(configured) != url.as_ref())
    {
        return Err(xcb_core::Error::Invalid("Clef endpoint").into());
    }
    Ok((Id::new(model)?, url))
}
pub fn effective_target(config: &JudgeConfig) -> Result<(Id, Option<String>)> {
    target(
        config,
        std::env::var(ACCOUNT_ENV).ok(),
        std::env::var(MODEL_ENV).ok(),
    )
}
pub fn token() -> Result<Option<Zeroizing<String>>> {
    token_from(|name| std::env::var(name).ok())
}
pub fn token_from(env: impl Fn(&str) -> Option<String>) -> Result<Option<Zeroizing<String>>> {
    let Some(token) = env(TOKEN_ENV).or_else(|| env(TOKEN_ALIAS_ENV)) else {
        return Ok(None);
    };
    if !judge::valid_judge_token(&token) {
        return Err(xcb_core::Error::Invalid("Cloudflare API token").into());
    }
    Ok(Some(Zeroizing::new(token)))
}

pub fn check_images(images: &[Value]) -> Result<()> {
    if images.len() > 4 {
        return Err(xcb_core::Error::Limit("Clef images").into());
    }
    let mut total = 0;
    for image in images {
        let (mime, encoded) = if let Some(url) = image.as_str() {
            if url.len() > MAX_IMAGE_BYTES.div_ceil(3) * 4 + 32 {
                return Err(xcb_core::Error::Limit("Clef image").into());
            }
            let (prefix, encoded) = url
                .split_once(',')
                .ok_or(xcb_core::Error::Invalid("embedded image"))?;
            let lower = prefix.to_ascii_lowercase();
            let mime = lower
                .strip_prefix("data:")
                .and_then(|s| s.strip_suffix(";base64"))
                .ok_or(xcb_core::Error::Invalid("embedded image"))?;
            (mime.to_owned(), encoded)
        } else {
            let object = image
                .as_object()
                .ok_or(xcb_core::Error::Invalid("embedded image"))?;
            if object.len() != 2 {
                return Err(xcb_core::Error::Invalid("embedded image").into());
            }
            (
                object
                    .get("content_type")
                    .and_then(Value::as_str)
                    .ok_or(xcb_core::Error::Invalid("image type"))?
                    .to_owned(),
                object
                    .get("base64")
                    .and_then(Value::as_str)
                    .ok_or(xcb_core::Error::Invalid("image base64"))?,
            )
        };
        let format = match mime.as_str() {
            "image/png" => image::ImageFormat::Png,
            "image/jpeg" => image::ImageFormat::Jpeg,
            "image/webp" => image::ImageFormat::WebP,
            _ => return Err(xcb_core::Error::Invalid("image type").into()),
        };
        if encoded.len() > MAX_IMAGE_BYTES.div_ceil(3) * 4 {
            return Err(xcb_core::Error::Limit("Clef image").into());
        }
        let data = STANDARD
            .decode(encoded)
            .map_err(|_| xcb_core::Error::Invalid("image base64"))?;
        if STANDARD.encode(&data) != encoded {
            return Err(xcb_core::Error::Invalid("image base64").into());
        }
        total += data.len();
        if data.len() > MAX_IMAGE_BYTES || total > MAX_TOTAL_IMAGE_BYTES {
            return Err(xcb_core::Error::Limit("Clef image bytes").into());
        }
        if image::guess_format(&data).ok() != Some(format) {
            return Err(xcb_core::Error::Invalid("image type mismatch").into());
        }
        let (width, height) = image::ImageReader::with_format(Cursor::new(&data), format)
            .into_dimensions()
            .map_err(|_| xcb_core::Error::Invalid("image dimensions"))?;
        if width == 0 || height == 0 || u64::from(width) * u64::from(height) > MAX_PIXELS {
            return Err(xcb_core::Error::Limit("Clef image pixels").into());
        }
        let mut reader = image::ImageReader::with_format(Cursor::new(&data), format);
        let mut limits = image::Limits::default();
        limits.max_alloc = Some(128 * 1024 * 1024);
        limits.max_image_width = Some(width);
        limits.max_image_height = Some(height);
        reader.limits(limits);
        reader
            .decode()
            .map_err(|_| xcb_core::Error::Invalid("image encoding"))?;
    }
    Ok(())
}

pub fn request(
    state: &Value,
    questions: &JudgeQuestions,
    model: &str,
    images: &[Value],
) -> Result<Vec<u8>> {
    judge::check_state(state)?;
    judge::check_questions(questions)?;
    if !matches!(model, "clef" | "clef-flash") {
        return Err(xcb_core::Error::Invalid("Clef model").into());
    }
    for (name, q) in questions {
        if name.is_empty()
            || name.len() > 100
            || !name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-'))
        {
            return Err(xcb_core::Error::Invalid("Clef question ID").into());
        }
        match q {
            judge::JudgeQuestion::Choice { criteria, .. } if criteria.len() < 2 => {
                return Err(xcb_core::Error::Invalid("Clef choice options").into());
            }
            judge::JudgeQuestion::Score { criteria, .. } if !(2..=10).contains(&criteria.len()) => {
                return Err(xcb_core::Error::Invalid("Clef score levels").into());
            }
            _ => (),
        }
    }
    check_images(images)?;
    let mut value = serde_json::json!({"model": model, "state": state, "questions": questions});
    if !images.is_empty() {
        value["images"] = Value::Array(images.to_vec());
    }
    let body = serde_json::to_vec(&value)?;
    if body.len() > MAX_BODY_BYTES {
        return Err(xcb_core::Error::Limit("Clef request body").into());
    }
    Ok(body)
}

const CLEF_ROUNDING_HALF_UNIT: f64 = 0.5 * 0.0001;
const ROUNDING_ARITHMETIC_EPSILON: f64 = 1e-12;
struct ProbabilityBounds {
    lower: f64,
    upper: f64,
}
fn rounded_score_endpoint(bounds: &[ProbabilityBounds], lower_total: f64, maximum: bool) -> f64 {
    let mut score = bounds
        .iter()
        .enumerate()
        .map(|(level, bound)| level as f64 * bound.lower)
        .sum::<f64>();
    let mut remaining = (1.0 - lower_total).max(0.0);
    for position in 0..bounds.len() {
        if remaining <= 0.0 {
            break;
        }
        let level = if maximum {
            bounds.len() - 1 - position
        } else {
            position
        };
        let bound = &bounds[level];
        let added = remaining.min(bound.upper - bound.lower);
        score += level as f64 * added;
        remaining -= added;
    }
    score
}

pub fn parse_response(
    status: u16,
    body: &[u8],
    model: &str,
    questions: &JudgeQuestions,
) -> Result<JudgeAnswers> {
    if !(200..300).contains(&status) {
        return crate::jev::parse_response(status, b"{}");
    }
    if body.len() > 256 * 1024 {
        return Err(xcb_core::Error::Limit("judge response").into());
    }
    let invalid = || Error::Unavailable("invalid Cloudflare Clef response");
    let envelope: Value = serde_json::from_slice(body).map_err(|_| invalid())?;
    if envelope.get("success").and_then(Value::as_bool) != Some(true)
        || !envelope
            .get("errors")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
    {
        return Err(invalid());
    }
    let raw = envelope.get("result").ok_or_else(invalid)?;
    let answers = crate::jev::parse_response(status, &serde_json::to_vec(raw)?)?;
    judge::check_answers(questions, &answers)?;
    if answers.model.as_deref() != Some(model) {
        return Err(invalid());
    }
    let usage = raw
        .get("usage")
        .and_then(Value::as_object)
        .ok_or_else(invalid)?;
    if usage.len() != 2
        || ["input_tokens", "output_tokens"]
            .iter()
            .any(|key| usage.get(*key).and_then(Value::as_u64).is_none())
    {
        return Err(invalid());
    }
    for (id, question) in questions {
        let raw_answer = &raw["answers"][id];
        let answer = &answers.answers[id];
        let kind = match answer {
            JudgeAnswer::Noul(_) => "noul",
            JudgeAnswer::Choice { .. } => "choice",
            JudgeAnswer::Score { .. } => "score",
        };
        if raw_answer.get("type").and_then(Value::as_str) != Some(kind) {
            return Err(invalid());
        }
        let (probabilities, expected) = match (answer, question) {
            (JudgeAnswer::Noul(_), _) => continue,
            (
                JudgeAnswer::Choice {
                    choice,
                    probabilities,
                    ..
                },
                judge::JudgeQuestion::Choice { criteria, .. },
            ) => {
                if probabilities.values().any(|p| p > &probabilities[choice]) {
                    return Err(invalid());
                }
                (probabilities, criteria.keys().cloned().collect::<Vec<_>>())
            }
            (
                JudgeAnswer::Score { probabilities, .. },
                judge::JudgeQuestion::Score { criteria, .. },
            ) => {
                let legend = raw_answer
                    .get("legend")
                    .and_then(Value::as_object)
                    .ok_or_else(invalid)?;
                if legend.len() != criteria.len()
                    || criteria
                        .iter()
                        .enumerate()
                        .any(|(i, c)| legend.get(&i.to_string()).and_then(Value::as_str) != Some(c))
                {
                    return Err(invalid());
                }
                (
                    probabilities,
                    (0..criteria.len()).map(|i| i.to_string()).collect(),
                )
            }
            _ => return Err(invalid()),
        };
        if probabilities.len() != expected.len()
            || expected.iter().any(|key| !probabilities.contains_key(key))
        {
            return Err(invalid());
        }
        let bounds = expected
            .iter()
            .map(|key| ProbabilityBounds {
                lower: (probabilities[key] - CLEF_ROUNDING_HALF_UNIT).max(0.0),
                upper: (probabilities[key] + CLEF_ROUNDING_HALF_UNIT).min(1.0),
            })
            .collect::<Vec<_>>();
        let lower_total = bounds.iter().map(|bound| bound.lower).sum::<f64>();
        let upper_total = bounds.iter().map(|bound| bound.upper).sum::<f64>();
        if lower_total > 1.0 + ROUNDING_ARITHMETIC_EPSILON
            || upper_total < 1.0 - ROUNDING_ARITHMETIC_EPSILON
        {
            return Err(invalid());
        }
        if let JudgeAnswer::Score { score, .. } = answer {
            let minimum = rounded_score_endpoint(&bounds, lower_total, false);
            let maximum = rounded_score_endpoint(&bounds, lower_total, true);
            if score + CLEF_ROUNDING_HALF_UNIT < minimum - ROUNDING_ARITHMETIC_EPSILON
                || score - CLEF_ROUNDING_HALF_UNIT > maximum + ROUNDING_ARITHMETIC_EPSILON
            {
                return Err(invalid());
            }
        }
    }
    Ok(answers)
}

pub struct Clef {
    transport: crate::jev::SystemOne,
    model: String,
}
impl Clef {
    pub fn new(token: Zeroizing<String>, account: &str, model: &str) -> Result<Self> {
        if !judge::valid_judge_token(&token) {
            return Err(xcb_core::Error::Invalid("Cloudflare API token").into());
        }
        Ok(Self {
            transport: crate::jev::SystemOne::transport(
                token,
                model.into(),
                endpoint(account, model)?,
            )?,
            model: model.into(),
        })
    }
}
impl Judge for Clef {
    fn ask<'a>(
        &'a self,
        state: &'a Value,
        questions: &'a JudgeQuestions,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<JudgeAnswers>> + Send + 'a>>
    {
        self.ask_with_images(state, questions, &[])
    }
    fn ask_with_images<'a>(
        &'a self,
        state: &'a Value,
        questions: &'a JudgeQuestions,
        images: &'a [Value],
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<JudgeAnswers>> + Send + 'a>>
    {
        Box::pin(async move {
            let body = request(state, questions, &self.model, images)?;
            let (status, response) =
                tokio::time::timeout(Duration::from_secs(15), self.transport.exchange(&body))
                    .await
                    .map_err(|_| Error::Unavailable("judge request timed out"))??;
            parse_response(status, &response, &self.model, questions)
        })
    }
}
pub fn resolve(config: &JudgeConfig) -> Result<Option<Arc<dyn Judge>>> {
    let (model, url) = effective_target(config)?;
    let Some(url) = url else {
        return Ok(None);
    };
    let Some(token) = token()? else {
        return Ok(None);
    };
    let account = url
        .strip_prefix("https://api.cloudflare.com/client/v4/accounts/")
        .and_then(|s| s.split('/').next())
        .ok_or(xcb_core::Error::Invalid("Cloudflare account ID"))?;
    Ok(Some(Arc::new(Clef::new(token, account, model.as_str())?)))
}

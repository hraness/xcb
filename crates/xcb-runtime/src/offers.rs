use crate::{Error, Result, digest, now_ms, private};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use xcb_core::{
    Provider,
    models::{Mode, ModelChoice},
};

const DEVIN_PRICING_URL: &str = "https://devin.ai/pricing";
const MAX_RESPONSE: usize = 2 * 1024 * 1024;
const REFRESH_MS: u64 = 6 * 60 * 60 * 1000;
const FRESH_MS: u64 = 24 * 60 * 60 * 1000;
const SWE2_PROMOTION_END_MS: u64 = 1_791_676_800_000;
const SWE2_TERMS: &str = "Free use of SWE-2 Free in Devin Desktop and CLI through October 10, 2026";
const SWE2_VARIANTS: &[&str] = &["swe-2-medium", "swe-2-high", "swe-2-max"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OfferKind {
    Free,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelOffer {
    pub provider: Provider,
    pub model_prefix: String,
    pub surface: String,
    pub kind: OfferKind,
    pub terms: String,
    pub valid_until_ms: u64,
    pub source: String,
}
impl ModelOffer {
    fn validate(&self) -> Result<()> {
        // This build understands one public promotion. A cached observation
        // cannot broaden it to another provider, variant, or end date.
        if self.provider != Provider::Devin
            || self.model_prefix != "swe-2-"
            || self.surface != "devin_cli"
            || self.terms != SWE2_TERMS
            || self.valid_until_ms != SWE2_PROMOTION_END_MS
            || self.source != DEVIN_PRICING_URL
        {
            return Err(xcb_core::Error::Invalid("model offer").into());
        }
        Ok(())
    }
    pub fn applies(&self, model: &ModelChoice, now: u64) -> bool {
        self.validate().is_ok()
            && self.provider == model.provider
            && model.mode == Mode::Fixed
            && model.effort.is_none()
            && SWE2_VARIANTS.contains(&model.id.as_str())
            && now < self.valid_until_ms
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OfferState {
    pub version: u32,
    pub checked_at_ms: u64,
    pub next_check_ms: u64,
    pub source_sha256: String,
    pub offers: Vec<ModelOffer>,
}
impl Default for OfferState {
    fn default() -> Self {
        Self {
            version: 1,
            checked_at_ms: 0,
            next_check_ms: 0,
            source_sha256: String::new(),
            offers: vec![],
        }
    }
}
impl OfferState {
    fn validate(&self) -> Result<()> {
        if self.version != 1
            || self.next_check_ms < self.checked_at_ms
            || self.next_check_ms > self.checked_at_ms.saturating_add(REFRESH_MS)
            || self.offers.len() > 1
            || if self.checked_at_ms == 0 {
                self.next_check_ms != 0 || !self.source_sha256.is_empty() || !self.offers.is_empty()
            } else {
                self.source_sha256.len() != 64
                    || !self
                        .source_sha256
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit())
            }
        {
            return Err(xcb_core::Error::Invalid("offer state").into());
        }
        for offer in &self.offers {
            offer.validate()?;
        }
        Ok(())
    }
    pub fn fresh(&self, now: u64) -> bool {
        self.validate().is_ok()
            && self.checked_at_ms > 0
            && self.source_sha256.len() == 64
            && self.checked_at_ms <= now
            && now.saturating_sub(self.checked_at_ms) <= FRESH_MS
    }
    pub fn offer_for<'a>(&'a self, model: &ModelChoice, now: u64) -> Option<&'a ModelOffer> {
        self.fresh(now)
            .then(|| self.offers.iter().find(|offer| offer.applies(model, now)))
            .flatten()
    }
}

fn path(root: &Path) -> PathBuf {
    root.join("offers.json")
}

pub fn load(root: &Path) -> Result<OfferState> {
    match private::read(&path(root), 64 * 1024) {
        Ok(bytes) => {
            let state: OfferState = serde_json::from_slice(&bytes).map_err(|_| {
                Error::Unavailable("offer state is incompatible with this xcb build")
            })?;
            state.validate()?;
            Ok(state)
        }
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(OfferState::default())
        }
        Err(error) => Err(error),
    }
}

fn save(root: &Path, state: &OfferState) -> Result<()> {
    state.validate()?;
    let bytes = serde_json::to_vec_pretty(state)?;
    match private::read(&path(root), 64 * 1024) {
        Ok(previous) => {
            if serde_json::from_slice::<OfferState>(&previous).is_ok_and(|old| {
                old.validate().is_ok()
                    && old.checked_at_ms > state.checked_at_ms
                    && old.checked_at_ms <= now_ms()
            }) {
                return Err(Error::Conflict("newer offer observation already saved"));
            }
            private::replace(&path(root), &bytes, &digest(previous))
        }
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            private::create(&path(root), &bytes)
        }
        Err(error) => Err(error),
    }
}

fn parse_devin_pricing(bytes: &[u8], now: u64) -> Result<OfferState> {
    if bytes.len() > MAX_RESPONSE {
        return Err(xcb_core::Error::Limit("offer response").into());
    }
    let html = std::str::from_utf8(bytes)
        .map_err(|_| xcb_core::Error::Invalid("offer response encoding"))?;
    // Script bundles and comments can retain withdrawn offers long after the
    // rendered pricing copy changed. They are not a fresh pricing observation.
    let ignored = regex::Regex::new(r"(?is)<!--.*?(?:-->|\z)|<(script|style|template)\b[^>]*>.*?(?:</(?:script|style|template)\s*>|\z)")
        .expect("static pricing content regex");
    let html = ignored.replace_all(html, " ");
    let mut text = String::with_capacity(html.len());
    let mut tag = false;
    for character in html.chars() {
        match character {
            '<' => {
                tag = true;
                text.push(' ');
            }
            '>' => {
                tag = false;
                text.push(' ');
            }
            _ if !tag => text.push(character),
            _ => (),
        }
    }
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let offers = (text.contains(SWE2_TERMS) && now < SWE2_PROMOTION_END_MS)
        .then_some(ModelOffer {
            provider: Provider::Devin,
            model_prefix: "swe-2-".into(),
            surface: "devin_cli".into(),
            kind: OfferKind::Free,
            terms: SWE2_TERMS.into(),
            valid_until_ms: SWE2_PROMOTION_END_MS,
            source: DEVIN_PRICING_URL.into(),
        })
        .into_iter()
        .collect();
    let state = OfferState {
        version: 1,
        checked_at_ms: now,
        next_check_ms: now.saturating_add(REFRESH_MS),
        source_sha256: digest(bytes),
        offers,
    };
    state.validate()?;
    Ok(state)
}

fn fetch_devin_pricing() -> Result<Vec<u8>> {
    // The synchronous API is also used from async CLI code. Keep its bounded
    // subprocess runtime on a separate thread instead of nesting runtimes.
    std::thread::spawn(|| {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let mut command = tokio::process::Command::new("/usr/bin/curl");
        command.env_clear().args([
            "--disable",
            "--fail",
            "--silent",
            "--show-error",
            "--location",
            "--max-redirs",
            "0",
            "--max-time",
            "8",
            "--max-filesize",
            "2097152",
            "--connect-timeout",
            "3",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--user-agent",
            "xcb-offers/0.4",
            DEVIN_PRICING_URL,
        ]);
        // The host enforces the byte/deadline limits and joins the process
        // group even when curl's version cannot bound a streamed response.
        runtime.block_on(crate::process::capture_with_input(
            command,
            &[],
            MAX_RESPONSE,
            Duration::from_secs(10),
        ))
    })
    .join()
    .map_err(|_| Error::Unavailable("official offer refresh worker failed"))?
}

pub fn refresh(root: &Path) -> Result<OfferState> {
    let state = parse_devin_pricing(&fetch_devin_pricing()?, now_ms())?;
    save(root, &state)?;
    Ok(state)
}

pub fn refresh_if_due(root: &Path, now: u64) -> Result<OfferState> {
    let state = load(root)?;
    if state.checked_at_ms <= now && state.next_check_ms > now {
        return Ok(state);
    }
    let next = parse_devin_pricing(&fetch_devin_pricing()?, now)?;
    save(root, &next)?;
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;
    use xcb_core::{Id, models::Mode};

    fn model(id: &str) -> ModelChoice {
        ModelChoice {
            provider: Provider::Devin,
            id: Id::new(id).unwrap(),
            label: id.into(),
            mode: Mode::Fixed,
            resolved: None,
            effort: None,
            observed_at_ms: 1,
        }
    }

    #[test]
    fn official_swe2_offer_is_scoped_fresh_and_self_expiring() {
        let body = "<main>Free use of <strong>SWE-2 Free</strong> in Devin Desktop and CLI through October 10, 2026</main>";
        let state = parse_devin_pricing(body.as_bytes(), 1_790_000_000_000).unwrap();
        assert!(
            state
                .offer_for(&model("swe-2-high"), 1_790_000_000_000)
                .is_some()
        );
        assert!(
            state
                .offer_for(&model("gpt-6-astra-high"), 1_790_000_000_000)
                .is_none()
        );
        assert!(
            state
                .offer_for(&model("swe-2-high"), SWE2_PROMOTION_END_MS)
                .is_none()
        );
        assert!(
            state
                .offer_for(&model("swe-2-high"), 1_790_100_000_001)
                .is_none()
        );
    }

    #[test]
    fn absent_or_changed_terms_never_imply_free_usage() {
        let state = parse_devin_pricing(b"SWE-2 is available", 100).unwrap();
        assert!(state.offers.is_empty());
        assert!(state.offer_for(&model("swe-2-high"), 100).is_none());
    }

    #[test]
    fn comments_scripts_and_templates_cannot_refresh_a_withdrawn_promotion() {
        for body in [
            format!("<!-- {SWE2_TERMS} -->"),
            format!("<script>const terms = '{SWE2_TERMS}';</script>"),
            format!("<SCRIPT type='text/javascript'>{SWE2_TERMS}</SCRIPT>"),
            format!("<style>{SWE2_TERMS}</style>"),
            format!("<template>{SWE2_TERMS}</template>"),
            format!("<script>{SWE2_TERMS}"),
        ] {
            assert!(
                parse_devin_pricing(body.as_bytes(), 100)
                    .unwrap()
                    .offers
                    .is_empty(),
                "{body}"
            );
        }
        assert!(
            parse_devin_pricing(SWE2_TERMS.as_bytes(), SWE2_PROMOTION_END_MS)
                .unwrap()
                .offers
                .is_empty()
        );
    }

    #[test]
    fn offer_never_expands_to_unknown_or_composite_model_variants() {
        let state = parse_devin_pricing(SWE2_TERMS.as_bytes(), 100).unwrap();
        for id in [
            "swe-2-premium",
            "swe-2-future",
            "swe-20-high",
            "fusion-swe-2-high",
        ] {
            assert!(state.offer_for(&model(id), 100).is_none());
        }
        let mut altered = model("swe-2-high");
        altered.provider = Provider::Codex;
        assert!(state.offer_for(&altered, 100).is_none());
        altered.provider = Provider::Devin;
        altered.mode = Mode::Fusion;
        assert!(state.offer_for(&altered, 100).is_none());
        altered.mode = Mode::Fixed;
        altered.effort = Some(Id::new("ultra").unwrap());
        assert!(state.offer_for(&altered, 100).is_none());
    }

    #[test]
    fn cached_observations_cannot_broaden_supported_terms_or_freshness() {
        let state = parse_devin_pricing(SWE2_TERMS.as_bytes(), 100).unwrap();
        for mutate in [
            (|s: &mut OfferState| s.offers[0].provider = Provider::Claude) as fn(&mut OfferState),
            |s| s.offers[0].model_prefix = "gpt-".into(),
            |s| s.offers[0].valid_until_ms += 1,
            |s| s.offers[0].terms = "all models are free".into(),
            |s| s.source_sha256 = "z".repeat(64),
            |s| s.next_check_ms = u64::MAX,
            |s| s.checked_at_ms = 0,
        ] {
            let mut invalid = state.clone();
            mutate(&mut invalid);
            assert!(invalid.validate().is_err());
            assert!(invalid.offer_for(&model("swe-2-high"), 100).is_none());
        }
        assert!(!state.fresh(99));
        assert!(state.fresh(100 + FRESH_MS));
        assert!(!state.fresh(101 + FRESH_MS));
    }

    #[test]
    fn older_refresh_cannot_overwrite_a_newer_saved_observation() {
        let directory = tempfile::tempdir().unwrap();
        let root =
            private::directory(&directory.path().canonicalize().unwrap().join("state")).unwrap();
        let old = parse_devin_pricing(SWE2_TERMS.as_bytes(), 100).unwrap();
        let new = parse_devin_pricing(b"Promotion withdrawn", 200).unwrap();
        save(&root, &new).unwrap();
        assert!(save(&root, &old).is_err());
        let retained = load(&root).unwrap();
        assert_eq!(retained.checked_at_ms, 200);
        assert!(retained.offers.is_empty());
    }

    #[test]
    fn pricing_response_size_and_encoding_are_bounded() {
        assert!(parse_devin_pricing(&vec![b'x'; MAX_RESPONSE + 1], 100).is_err());
        assert!(parse_devin_pricing(&[0xff], 100).is_err());
    }
}

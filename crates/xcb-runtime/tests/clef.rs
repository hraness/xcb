use base64::{Engine, engine::general_purpose::STANDARD};
use std::collections::BTreeMap;
use xcb_runtime::{
    clef,
    config::JudgeConfig,
    judge::{JudgeQuestion, JudgeQuestions},
};

fn crc(data: &[u8]) -> u32 {
    let mut value = u32::MAX;
    for byte in data {
        value ^= u32::from(*byte);
        for _ in 0..8 {
            value = (value >> 1) ^ if value & 1 == 1 { 0xedb88320 } else { 0 };
        }
    }
    value ^ u32::MAX
}

fn questions() -> JudgeQuestions {
    BTreeMap::from([(
        "score".into(),
        JudgeQuestion::Score {
            instructions: "Rate".into(),
            criteria: vec!["low".into(), "high".into()],
        },
    )])
}
fn response() -> serde_json::Value {
    serde_json::json!({"success":true,"errors":[],"result":{
        "model":"clef","usage":{"input_tokens":10,"output_tokens":2},
        "answers":{"score":{"type":"score","score":0.8,"confidence":0.8,
        "probabilities":{"0":0.2,"1":0.8},"legend":{"0":"low","1":"high"}}}}})
}
#[test]
fn clef_accepts_recorded_provider_rounding() {
    let q: JudgeQuestions = serde_json::from_value(serde_json::json!({
        "urgent": {"type":"noul","instructions":"Is the outage urgent?"},
        "team": {"type":"choice","instructions":"Which team should handle the outage?",
            "criteria":{"technical":"Outages and errors","sales":"Sales inquiries"}},
        "severity": {"type":"score","instructions":"How severe is the customer impact?",
            "criteria":["No impact","Minor","Major","Critical"]}
    }))
    .unwrap();
    for recorded in [
        serde_json::json!({"model":"clef","usage":{"input_tokens":319,"output_tokens":0},"answers":{
            "urgent":{"type":"noul","noul":0.9869},
            "team":{"type":"choice","choice":"technical","probabilities":{"technical":0.9635,"sales":0.0365},"confidence":0.8593},
            "severity":{"type":"score","score":2.931,"legend":{"0":"No impact","1":"Minor","2":"Major","3":"Critical"},
                "probabilities":{"0":0.0047,"1":0.0051,"2":0.0448,"3":0.9454},"confidence":0.8612}
        }}),
        serde_json::json!({"model":"clef-flash","usage":{"input_tokens":319,"output_tokens":0},"answers":{
            "urgent":{"type":"noul","noul":0.9354},
            "team":{"type":"choice","choice":"technical","probabilities":{"technical":0.9724,"sales":0.0276},"confidence":0.8928},
            "severity":{"type":"score","score":2.7378,"legend":{"0":"No impact","1":"Minor","2":"Major","3":"Critical"},
                "probabilities":{"0":0.0149,"1":0.0157,"2":0.186,"3":0.7834},"confidence":0.5316}
        }}),
    ] {
        let model = recorded["model"].as_str().unwrap();
        let envelope = serde_json::json!({"success":true,"errors":[],"result":recorded});
        let parsed =
            clef::parse_response(200, &serde_json::to_vec(&envelope).unwrap(), model, &q).unwrap();
        assert_eq!(parsed.model.as_deref(), Some(model));
        assert_eq!(
            parsed.answers["urgent"].noul(),
            recorded["answers"]["urgent"]["noul"].as_f64()
        );
        assert_eq!(
            parsed.answers["team"].choice(),
            Some((
                "technical",
                recorded["answers"]["team"]["confidence"].as_f64().unwrap()
            ))
        );
        assert_eq!(
            parsed.answers["severity"].score(),
            Some((
                recorded["answers"]["severity"]["score"].as_f64().unwrap(),
                recorded["answers"]["severity"]["confidence"]
                    .as_f64()
                    .unwrap()
            ))
        );
    }
}
#[test]
fn clef_targets_are_fixed_and_defaults_do_not_reuse_legacy_auth() {
    let config = JudgeConfig::default();
    assert!(config.is_clef());
    assert_eq!(
        clef::endpoint(&"a".repeat(32), "clef-flash").unwrap(),
        format!(
            "https://api.cloudflare.com/client/v4/accounts/{}/ai/run/@cf/cloudflare/clef-flash",
            "a".repeat(32)
        )
    );
    assert!(clef::endpoint("invalid", "clef").is_err());
    assert!(clef::endpoint(&"a".repeat(32), "jev-latest").is_err());
    assert!(
        clef::token_from(|name| (name == "TYPESAFE_API_KEY").then(|| "legacy-key".into()))
            .unwrap()
            .is_none()
    );
    assert_eq!(
        clef::token_from(|name| (name == clef::TOKEN_ALIAS_ENV).then(|| "alias".into()))
            .unwrap()
            .unwrap()
            .as_str(),
        "alias"
    );
    assert!(
        clef::token_from(|name| match name {
            clef::TOKEN_ENV => Some("invalid token".into()),
            clef::TOKEN_ALIAS_ENV => Some("valid-alias".into()),
            _ => None,
        })
        .is_err()
    );
    let (model, url) = clef::target(&config, None, None).unwrap();
    assert_eq!(model.as_str(), "clef");
    assert!(url.is_none());
    let legacy: JudgeConfig =
        serde_json::from_value(serde_json::json!({"enabled":true,"model":"jev-latest"})).unwrap();
    assert!(!legacy.is_clef());
    legacy.validate().unwrap();
    let encoded = serde_json::to_value(&legacy).unwrap();
    assert_eq!(encoded["model"], "jev-latest");
    assert!(encoded.get("provider").is_none());
    for config in [
        serde_json::json!({"provider":"clef","model":"jev-latest"}),
        serde_json::json!({"provider":"clef","endpoint":"https://example.com"}),
        serde_json::json!({"account_id":"invalid"}),
    ] {
        assert!(
            !serde_json::from_value::<JudgeConfig>(config)
                .is_ok_and(|config| config.validate().is_ok())
        );
    }
    assert!(
        xcb_runtime::judge::check_key_target(
            xcb_runtime::judge::JudgeKeySource::Vault,
            &JudgeConfig::default()
        )
        .is_err()
    );
}
#[test]
fn clef_requires_rest_envelope_exact_model_and_original_legend() {
    let q = questions();
    let valid = response();
    clef::parse_response(200, &serde_json::to_vec(&valid).unwrap(), "clef", &q).unwrap();
    let mut wrong_model = valid.clone();
    wrong_model["result"]["model"] = "clef-flash".into();
    let mut wrong_legend = valid.clone();
    wrong_legend["result"]["answers"]["score"]["legend"]["0"] = "changed".into();
    let mut wrong_score = valid.clone();
    wrong_score["result"]["answers"]["score"]["score"] = 1.into();
    let mut error = valid.clone();
    error["errors"] = serde_json::json!([{"message":"private"}]);
    for value in [
        valid["result"].clone(),
        wrong_model,
        wrong_legend,
        wrong_score,
        error,
    ] {
        assert!(
            clef::parse_response(200, &serde_json::to_vec(&value).unwrap(), "clef", &q).is_err()
        );
    }
    assert!(clef::parse_response(429, b"{}", "clef", &q).is_err());
}
#[test]
fn clef_rounding_requires_normalization_and_an_attainable_score() {
    for (zero, one, score, accepted) in [
        (0.2, 0.7999, 0.8, true),
        (0.2, 0.8001, 0.8, true),
        (0.0, 1.0, 0.9999, true),
        (0.2001, 0.8001, 0.8, false),
        (0.1999, 0.7999, 0.8, false),
        (0.2, 0.8, 0.80011, false),
        (0.2, 0.8001, 0.7999, false),
        (0.0, 1.0, 0.99989, false),
        (0.0, 0.99989, 0.9999, false),
        (0.0, 0.0, 0.0, false),
    ] {
        let mut value = response();
        value["result"]["answers"]["score"]["probabilities"] =
            serde_json::json!({"0":zero,"1":one});
        value["result"]["answers"]["score"]["score"] = score.into();
        assert_eq!(
            clef::parse_response(
                200,
                &serde_json::to_vec(&value).unwrap(),
                "clef",
                &questions()
            )
            .is_ok(),
            accepted,
            "zero={zero}, one={one}, score={score}"
        );
    }
    let q: JudgeQuestions = serde_json::from_value(serde_json::json!({
        "pick":{"type":"choice","instructions":"Which?","criteria":{"a":"first","b":"second"}}
    }))
    .unwrap();
    for (a, b, accepted) in [
        (0.2, 0.7999, true),
        (0.2, 0.8001, true),
        (0.2001, 0.8001, false),
        (0.1999, 0.7999, false),
        (0.0, 0.99989, false),
    ] {
        let value = serde_json::json!({"success":true,"errors":[],"result":{
            "model":"clef","usage":{"input_tokens":10,"output_tokens":0},
            "answers":{"pick":{"type":"choice","choice":"b","confidence":0.8,
                "probabilities":{"a":a,"b":b}}}
        }});
        assert_eq!(
            clef::parse_response(200, &serde_json::to_vec(&value).unwrap(), "clef", &q).is_ok(),
            accepted,
            "a={a}, b={b}"
        );
    }
}

#[test]
fn clef_accepts_rounded_normalized_distributions_across_score_levels() {
    let round = |value: f64| (value * 10_000.0).round() / 10_000.0;
    for levels in 2..=10 {
        for seed in 0..16 {
            let weights = (0..levels)
                .map(|level| ((seed * 7 + level * 11) % 17) as f64)
                .collect::<Vec<_>>();
            let total = weights.iter().sum::<f64>();
            let original = weights
                .iter()
                .map(|weight| weight / total)
                .collect::<Vec<_>>();
            let criteria = (0..levels)
                .map(|level| format!("level {level}"))
                .collect::<Vec<_>>();
            let legend = criteria
                .iter()
                .enumerate()
                .map(|(level, criterion)| (level.to_string(), criterion.clone()))
                .collect::<BTreeMap<_, _>>();
            let probabilities = original
                .iter()
                .enumerate()
                .map(|(level, probability)| (level.to_string(), round(*probability)))
                .collect::<BTreeMap<_, _>>();
            let score = round(
                original
                    .iter()
                    .enumerate()
                    .map(|(level, probability)| level as f64 * probability)
                    .sum(),
            );
            let q = BTreeMap::from([(
                "score".into(),
                JudgeQuestion::Score {
                    instructions: "Rate".into(),
                    criteria,
                },
            )]);
            let value = serde_json::json!({"success":true,"errors":[],"result":{
                "model":"clef","usage":{"input_tokens":1,"output_tokens":0},
                "answers":{"score":{"type":"score","score":score,"confidence":0.5,
                    "probabilities":probabilities,"legend":legend}}
            }});
            let parsed =
                clef::parse_response(200, &serde_json::to_vec(&value).unwrap(), "clef", &q)
                    .unwrap();
            assert_eq!(parsed.answers["score"].score(), Some((score, 0.5)));
        }
    }
}

#[test]
fn clef_accepts_embedded_formats_and_rejects_pixel_byte_and_body_limits() {
    use image::ImageEncoder;
    for (mime, format) in [
        ("image/png", image::ImageFormat::Png),
        ("image/jpeg", image::ImageFormat::Jpeg),
        ("image/webp", image::ImageFormat::WebP),
    ] {
        let mut data = Vec::new();
        match format {
            image::ImageFormat::Png => image::codecs::png::PngEncoder::new(&mut data)
                .write_image(&[255, 0, 0], 1, 1, image::ExtendedColorType::Rgb8)
                .unwrap(),
            image::ImageFormat::Jpeg => image::codecs::jpeg::JpegEncoder::new(&mut data)
                .write_image(&[255, 0, 0], 1, 1, image::ExtendedColorType::Rgb8)
                .unwrap(),
            _ => image::codecs::webp::WebPEncoder::new_lossless(&mut data)
                .write_image(&[255, 0, 0], 1, 1, image::ExtendedColorType::Rgb8)
                .unwrap(),
        }
        clef::check_images(&[
            serde_json::json!({"content_type":mime,"base64":STANDARD.encode(&data)}),
        ])
        .unwrap();
        if format == image::ImageFormat::Png {
            let mut pixels = data.clone();
            pixels[16..20].copy_from_slice(&4001_u32.to_be_bytes());
            pixels[20..24].copy_from_slice(&4000_u32.to_be_bytes());
            let checksum = crc(&pixels[12..29]);
            pixels[29..33].copy_from_slice(&checksum.to_be_bytes());
            assert!(
                clef::check_images(&[
                    serde_json::json!({"content_type":mime,"base64":STANDARD.encode(pixels)})
                ])
                .is_err()
            );
            let size = 3 * 1024 * 1024;
            let mut padded = data[..33].to_vec();
            let mut chunk = vec![0_u8; size + 12];
            chunk[..4].copy_from_slice(&(size as u32).to_be_bytes());
            chunk[4..8].copy_from_slice(b"raNd");
            let checksum = crc(&chunk[4..size + 8]);
            chunk[size + 8..].copy_from_slice(&checksum.to_be_bytes());
            padded.extend(chunk);
            padded.extend(&data[33..]);
            let image = serde_json::json!({"content_type":mime,"base64":STANDARD.encode(padded)});
            clef::check_images(std::slice::from_ref(&image)).unwrap();
            assert!(clef::check_images(&[image.clone(), image.clone(), image]).is_err());
        }
    }
    assert!(clef::check_images(&[serde_json::json!({"content_type":"image/png","base64":"A".repeat(clef::MAX_IMAGE_BYTES.div_ceil(3)*4+4)})]).is_err());
    let questions = (0..64)
        .map(|i| {
            (
                format!("q{i}"),
                JudgeQuestion::Choice {
                    instructions: "Choose".into(),
                    criteria: (0..64)
                        .map(|j| (format!("o{j}"), Some("x".repeat(4096))))
                        .collect(),
                },
            )
        })
        .collect();
    assert!(clef::request(&serde_json::json!("evidence"), &questions, "clef", &[]).is_err());
}

#[test]
fn clef_images_reject_remote_invalid_and_oversized_evidence() {
    let png = serde_json::json!(
        "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg=="
    );
    let body = clef::request(
        &serde_json::json!("evidence"),
        &questions(),
        "clef",
        std::slice::from_ref(&png),
    )
    .unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["images"],
        serde_json::json!([png])
    );
    for images in [
        serde_json::json!(["https://example.com/a.png"]),
        serde_json::json!(["data:image/png;base64,AAAA"]),
        serde_json::json!([png, png, png, png, png]),
    ] {
        assert!(
            clef::request(
                &serde_json::json!("evidence"),
                &questions(),
                "clef",
                images.as_array().unwrap()
            )
            .is_err()
        );
    }
}

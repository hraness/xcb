//! Native port of ALGAL's `examples/model-router.algal.json` fitted classifier.
//! Source: https://github.com/hraness/algal/blob/7bedd93/examples/model-router.algal.json
//! Provenance and limitations: ALGAL `docs/model-router.md` (306 first prompts;
//! preference prediction, not a capability benchmark). Keep the six questions,
//! literal-space features, coefficients, raw score scale and kind gates aligned.
//! Large-prompt priority is an explicit xcb policy layered above that model.

use crate::judge::{self, Judge, JudgeAnswers, JudgeQuestions};
use std::{collections::BTreeSet, time::Duration};

pub(crate) const TIMEOUT: Duration = Duration::from_secs(5);
const MAX_PROMPT_BYTES: usize = 32_768;
const VERBS: &[&str] = &[
    "run",
    "create",
    "fix",
    "add",
    "build",
    "make",
    "take",
    "resume",
    "continue",
    "check",
    "review",
    "audit",
    "deploy",
    "merge",
    "push",
    "migrate",
    "refactor",
    "test",
    "write",
    "update",
    "remove",
    "delete",
    "install",
    "setup",
    "set",
    "configure",
    "read",
    "search",
    "find",
    "analyze",
    "improve",
    "change",
    "edit",
    "implement",
    "design",
    "ship",
    "launch",
    "clean",
    "move",
    "rename",
    "upgrade",
    "inspect",
    "verify",
    "publish",
    "integrate",
    "combine",
    "unify",
    "port",
    "rewrite",
    "rework",
    "overhaul",
    "assess",
    "evaluate",
    "compare",
    "investigate",
    "debug",
    "diagnose",
    "trace",
    "profile",
    "optimize",
    "benchmark",
    "scrape",
    "extract",
    "sync",
    "import",
    "export",
    "generate",
];
const QUESTIONS_JSON: &str = r#"{
  "difficulty": {
    "type": "score",
    "instructions": "Rate the intrinsic difficulty of completing this request correctly for a strong coding agent: 1 = trivial lookup or one-line change, 2 = routine small task, 3 = multi-step but standard, 4 = substantial design or debugging effort, 5 = novel/architectural/highest difficulty.",
    "criteria": [
      "1",
      "2",
      "3",
      "4",
      "5"
    ]
  },
  "scope": {
    "type": "score",
    "instructions": "Rate the expected size of the work implied by the request: 1 = single command or tiny edit, 2 = one file/one function, 3 = several files or a small feature, 4 = a subsystem or cross-repo change, 5 = program/architecture spanning many components or repos.",
    "criteria": [
      "1",
      "2",
      "3",
      "4",
      "5"
    ]
  },
  "ambiguity": {
    "type": "score",
    "instructions": "Rate how ambiguous or under-specified the request is: 1 = exact and self-contained, 3 = intent clear but details open, 5 = requires substantial interpretation or missing context to even start.",
    "criteria": [
      "1",
      "2",
      "3",
      "4",
      "5"
    ]
  },
  "stakes": {
    "type": "score",
    "instructions": "Rate the cost of a wrong or sloppy outcome: 1 = throwaway/local and easy to verify, 3 = real work product with reviewable mistakes, 5 = production, money, data loss, or hard-to-reverse consequences.",
    "criteria": [
      "1",
      "2",
      "3",
      "4",
      "5"
    ]
  },
  "kind": {
    "type": "choice",
    "instructions": "Pick the single dominant kind of this request.",
    "criteria": {
      "feature": "build or extend functionality",
      "bugfix": "fix a defect or broken behavior",
      "refactor": "restructure code without new behavior",
      "research": "investigate, analyze, or report; no deliverable artifact required",
      "ops": "deploy, configure, migrate, or environment/infra work",
      "writing": "prose, docs, marketing, or editorial output",
      "question": "answer a question or explain; little or no code",
      "chore": "mechanical, repetitive, or housekeeping work",
      "resume": "continue or take over earlier work from a previous session",
      "probe": "capability test or smoke check of the model itself"
    }
  },
  "frontier": {
    "type": "noul",
    "instructions": "Probability that this request warrants a frontier-class model (top-tier capability) rather than a strong standard model. Consider difficulty, scope, ambiguity, and stakes \u2014 not prompt politeness or length."
  }
}"#;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Classification {
    pub frontier: bool,
    pub source: &'static str,
    pub score_milli: Option<i32>,
    pub kind: Option<String>,
}

impl Classification {
    pub fn reason(&self) -> String {
        match (&self.kind, self.score_milli) {
            (Some(kind), Some(score)) => format!("{} · {kind} · score {score}", self.source),
            _ => self.source.into(),
        }
    }
}

pub(crate) fn substantial(task: &str) -> bool {
    task.len() >= 8192 || task.split_whitespace().take(400).count() >= 400
}

fn questions() -> JudgeQuestions {
    serde_json::from_str(QUESTIONS_JSON).expect("static ALGAL classifier questions")
}

fn score(task: &str, answers: &JudgeAnswers) -> Option<Classification> {
    judge::check_answers(&questions(), answers).ok()?;
    // ALGAL splits on literal spaces, lowercases words without stripping
    // punctuation, and counts distinct original word spellings before capping.
    let words: Vec<_> = task.split(' ').filter(|word| !word.is_empty()).collect();
    let first = words.first().copied().unwrap_or("").to_lowercase();
    let lower = task.to_lowercase();
    let imperative = VERBS.contains(&first.as_str());
    let resume = ["resume", "continue"].contains(&first.as_str())
        || [
            "session named",
            "session called",
            "take over",
            "pick up",
            "continue the work",
        ]
        .iter()
        .any(|cue| lower.contains(cue));
    let verbs = words
        .iter()
        .copied()
        .filter(|word| VERBS.contains(&word.to_lowercase().as_str()))
        .collect::<BTreeSet<_>>()
        .len()
        .min(8);
    let answer = |name: &str| answers.answers.get(name);
    let scalar = |name: &str| answer(name)?.score().map(|(value, _)| value);
    let kind = answer("kind")?.choice()?.0;
    // Score answers are used verbatim, exactly as in the manifest. The native
    // Judge wire contract validates their five-criterion index range 0..=4.
    let z = -1.8027
        + 0.4530 * words.len().min(400) as f64 / 400.0
        + 1.5943 * u8::from(imperative) as f64
        + 0.6881 * u8::from(resume) as f64
        + 0.4405 * verbs as f64 / 8.0
        - 0.0755 * scalar("difficulty")? / 5.0
        + 0.8716 * scalar("scope")? / 5.0
        - 0.1508 * scalar("ambiguity")? / 5.0
        - 0.4787 * scalar("stakes")? / 5.0
        + 0.5851 * answer("frontier")?.noul()?;
    let kind_gate = ["question", "probe"].contains(&kind);
    Some(Classification {
        frontier: !kind_gate && z >= -0.619,
        source: "ALGAL fitted classifier",
        score_milli: Some((z * 1000.0).round() as i32),
        kind: Some(kind.into()),
    })
}

pub(crate) async fn classify(
    task: &str,
    backend: Option<&dyn Judge>,
    fallback_frontier: bool,
) -> Classification {
    let fallback = |source| Classification {
        frontier: fallback_frontier,
        source,
        score_milli: None,
        kind: None,
    };
    // This guarantee is independent of remote availability and cannot be
    // demoted by a classifier, including its historical question/probe gate.
    if substantial(task) {
        return Classification {
            frontier: true,
            ..fallback("large prompt · highest available quality")
        };
    }
    let Some(backend) = backend else {
        return fallback("deterministic fallback · classifier not available");
    };
    let state = serde_json::json!({"task": xcb_core::display_text(task, MAX_PROMPT_BYTES)});
    let questions = questions();
    match tokio::time::timeout(TIMEOUT, backend.ask(&state, &questions)).await {
        Ok(Ok(answers)) => score(task, &answers)
            .unwrap_or_else(|| fallback("deterministic fallback · invalid classifier response")),
        Ok(Err(_)) => fallback("deterministic fallback · classifier unavailable"),
        Err(_) => fallback("deterministic fallback · classifier timed out"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Result, judge::JudgeAnswer};
    use std::{collections::BTreeMap, future::Future, pin::Pin};

    fn answers(kind: &str) -> JudgeAnswers {
        let mut values = BTreeMap::new();
        for (key, value) in [
            ("difficulty", 3.9),
            ("scope", 3.8),
            ("ambiguity", 2.4),
            ("stakes", 2.9),
        ] {
            values.insert(
                key.into(),
                JudgeAnswer::Score {
                    score: value,
                    confidence: 0.8,
                    probabilities: BTreeMap::from([("3".into(), 1.0)]),
                },
            );
        }
        values.insert(
            "kind".into(),
            JudgeAnswer::Choice {
                choice: kind.into(),
                confidence: 1.0,
                probabilities: BTreeMap::from([(kind.into(), 1.0)]),
            },
        );
        values.insert("frontier".into(), JudgeAnswer::Noul(0.62));
        JudgeAnswers {
            answers: values,
            model: None,
        }
    }

    #[test]
    fn fitted_head_matches_algal_features_and_coefficients() {
        let task = "migrate the session store to the new envelope format and update every caller";
        let result = score(task, &answers("refactor")).unwrap();
        assert!(result.frontier);
        assert_eq!(result.score_milli, Some(533));
        for kind in ["question", "probe"] {
            let result = score(task, &answers(kind)).unwrap();
            assert!(!result.frontier);
            assert_eq!(result.score_milli, Some(533));
        }
        // Preserve the manifest's literal-space and punctuation behavior.
        assert_ne!(
            score("fix bug", &answers("bugfix")),
            score("fix, bug", &answers("bugfix"))
        );
    }

    #[test]
    fn incomplete_and_nonfinite_answers_cannot_route() {
        let mut invalid = answers("feature");
        invalid.answers.remove("scope");
        assert!(score("build it", &invalid).is_none());
        invalid = answers("feature");
        invalid
            .answers
            .insert("frontier".into(), JudgeAnswer::Noul(f64::NAN));
        assert!(score("build it", &invalid).is_none());
    }

    #[test]
    fn substantial_prompt_policy_has_exact_boundaries() {
        assert!(!substantial(&"word\n".repeat(399)));
        assert!(substantial(&"word\n".repeat(400)));
        assert!(!substantial(&"x".repeat(8191)));
        assert!(substantial(&"x".repeat(8192)));
    }

    struct FakeJudge(bool);
    impl Judge for FakeJudge {
        fn ask<'a>(
            &'a self,
            _: &'a serde_json::Value,
            _: &'a JudgeQuestions,
        ) -> Pin<Box<dyn Future<Output = Result<JudgeAnswers>> + Send + 'a>> {
            Box::pin(async move {
                if self.0 {
                    std::future::pending().await
                } else {
                    Ok(answers("question"))
                }
            })
        }
    }

    #[tokio::test]
    async fn missing_judge_is_honest_and_large_prompts_skip_judgment() {
        let fallback = classify("fix the bug", None, true).await;
        assert!(fallback.frontier);
        assert!(fallback.source.contains("not available"));
        assert!(
            !classify("a question", Some(&FakeJudge(false)), true)
                .await
                .frontier
        );
        let task = "word ".repeat(400);
        let result = tokio::time::timeout(
            Duration::from_millis(100),
            classify(&task, Some(&FakeJudge(true)), false),
        )
        .await
        .unwrap();
        assert!(result.frontier);
        assert!(result.source.contains("large prompt"));
    }

    #[tokio::test]
    async fn stalled_classifier_returns_a_bounded_deterministic_fallback() {
        let result = tokio::time::timeout(
            TIMEOUT + Duration::from_secs(2),
            classify("fix the bug", Some(&FakeJudge(true)), true),
        )
        .await
        .unwrap();
        assert!(result.frontier);
        assert!(result.source.contains("timed out"));
    }
}

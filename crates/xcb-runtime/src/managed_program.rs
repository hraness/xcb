//! Pinned, effect-free ALGAL planners for managed project schedules.
//!
//! The VM has no host executors, transports, imports, or persistent store. Its
//! only product is a bounded report and an optional proposed prompt. The caller
//! owns durable occurrence identity and must admit that prompt through the same
//! project policy as every other backlog proposal.

use crate::{Error, Result};
use algal::{
    canonical,
    contract::{Manifest, check_value},
    effects::Host,
    graph,
    store::Store,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, time::Duration};
use tokio::sync::watch;

pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;
pub const MAX_INPUT_BYTES: usize = 32 * 1024;
pub const MAX_SUMMARY_BYTES: usize = 8 * 1024;
pub const MAX_PROMPT_BYTES: usize = 8 * 1024;
const MAX_RUN_TIME: Duration = Duration::from_secs(5);

/// All source bytes needed to replay an occurrence, independent of its original
/// path. Deserialization is not admission: `run` always revalidates the snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdmittedProgram {
    pub manifest: Value,
    pub inputs: Value,
    pub manifest_digest: String,
    pub inputs_digest: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProgramReport {
    pub summary: String,
    pub prompt: Option<String>,
    pub receipt_digest: String,
}

fn invalid() -> Error {
    Error::Unavailable("program is outside the bounded ALGAL planner contract")
}

fn canonical_digest(value: &Value, max: usize) -> Result<String> {
    let bytes = canonical::canonical(value).map_err(|_| invalid())?;
    if bytes.len() > max {
        return Err(invalid());
    }
    canonical::digest(value).map_err(|_| invalid())
}

impl AdmittedProgram {
    pub fn admit(mut manifest: Value, inputs: Value) -> Result<Self> {
        // Omitted budgets get explicit planner defaults before the snapshot is
        // pinned. Explicitly larger budgets are rejected, never silently trusted.
        canonical_digest(&manifest, MAX_MANIFEST_BYTES)?;
        let object = manifest.as_object_mut().ok_or_else(invalid)?;
        let budgets = object.entry("budgets").or_insert_with(|| json!({}));
        let budgets = budgets.as_object_mut().ok_or_else(invalid)?;
        for (key, ceiling) in [
            ("maxSteps", 64),
            ("maxAgentCalls", 0),
            ("maxWork", 100_000),
            ("maxContextBytes", 32_768),
            ("maxOutputBytes", 16_384),
            ("maxDepth", 0),
        ] {
            let value = budgets.entry(key).or_insert(json!(ceiling));
            if value.as_u64().is_none_or(|n| n > ceiling) {
                return Err(invalid());
            }
        }
        let program = Self {
            manifest_digest: canonical_digest(&manifest, MAX_MANIFEST_BYTES)?,
            inputs_digest: canonical_digest(&inputs, MAX_INPUT_BYTES)?,
            manifest,
            inputs,
        };
        program.validated()?;
        Ok(program)
    }

    pub fn verify(&self) -> Result<()> {
        self.validated().map(|_| ())
    }

    fn validated(&self) -> Result<(Manifest, Value)> {
        if canonical_digest(&self.manifest, MAX_MANIFEST_BYTES)? != self.manifest_digest
            || canonical_digest(&self.inputs, MAX_INPUT_BYTES)? != self.inputs_digest
        {
            return Err(Error::Conflict("pinned program content changed"));
        }
        let manifest = Manifest::parse(&self.manifest).map_err(|_| invalid())?;
        let budgets = &manifest.budgets;
        if manifest.cells.len() > 32
            || manifest.edges.len() > 128
            || budgets.max_steps > 64
            || budgets.max_agent_calls != 0
            || budgets.max_work > 100_000
            || budgets.max_context_bytes > MAX_INPUT_BYTES
            || budgets.max_output_bytes > 16_384
            || budgets.max_depth != 0
        {
            return Err(invalid());
        }
        canonical_digest(&self.inputs, budgets.max_context_bytes.min(MAX_INPUT_BYTES))?;
        for cell in &manifest.cells {
            if !matches!(
                cell["kind"].as_str(),
                Some("input" | "const" | "fn" | "expr")
            ) {
                return Err(invalid());
            }
            // The pinned registry's functions are pure, including bounded
            // memory.query.v1 and context.compact.v1. Compilation rejects names
            // not supplied by that registry; no host tool signatures are given.
        }
        let compiled = graph::compile(
            manifest.clone(),
            &mut Store::default(),
            &BTreeMap::new(),
            &BTreeMap::new(),
            0,
        )
        .map_err(|_| invalid())?;
        let signature = graph::interface_signature(&compiled).map_err(|_| invalid())?;
        if signature
            .outputs
            .get("summary")
            .is_none_or(|p| p["type"] != "text")
            || signature.outputs.iter().any(|(name, port)| {
                !matches!(name.as_str(), "summary" | "prompt")
                    || port["type"] != "text"
                    || port["many"] == true
            })
        {
            return Err(invalid());
        }
        let inputs = self.inputs.as_object().ok_or_else(invalid)?;
        if inputs
            .keys()
            .any(|name| !signature.inputs.contains_key(name))
        {
            return Err(invalid());
        }
        for (name, port) in &signature.inputs {
            if matches!(port["type"].as_str(), Some("ref" | "cap")) {
                return Err(invalid());
            }
            match inputs.get(name) {
                Some(value) => check_value(port, value).map_err(|_| invalid())?,
                None if port["optional"] == true => (),
                None => return Err(invalid()),
            }
        }
        let args = graph::interface_args(&manifest, &self.inputs).map_err(|_| invalid())?;
        for cell in &manifest.cells {
            if cell["kind"] == "input" {
                for (port, declaration) in cell["outputs"].as_object().ok_or_else(invalid)? {
                    if args[cell["id"].as_str().ok_or_else(invalid)?]
                        .get(port)
                        .is_none()
                        && declaration["optional"] != true
                    {
                        return Err(invalid());
                    }
                }
            }
        }
        Ok((manifest, args))
    }

    /// CPU evaluation runs off the async supervisor. Cancellation discards its
    /// output but still joins bounded pure work; dropping a future cannot leave
    /// a provider, subprocess, or external effect running. ALGAL enforces graph,
    /// work, output, and expression-fuel bounds inside the interpreter.
    pub async fn run(&self, cancel: watch::Receiver<bool>) -> Result<ProgramReport> {
        if *cancel.borrow() {
            return Err(Error::Unavailable("program cancelled before execution"));
        }
        let (manifest, args) = self.validated()?;
        let started = std::time::Instant::now();
        let report = tokio::task::spawn_blocking(move || {
            let runtime = tokio::runtime::Builder::new_current_thread().build()?;
            let mut store = Store::default();
            let receipt = runtime
                .block_on(algal::runtime::run(
                    manifest.clone(),
                    args,
                    &mut store,
                    &mut Host::default(),
                    &BTreeMap::new(),
                    None,
                ))
                .map_err(|_| Error::Unavailable("bounded ALGAL program failed"))?;
            if receipt["outcome"] != "complete" {
                return Err(Error::Unavailable("bounded ALGAL program did not complete"));
            }
            let outputs = algal::runtime::outputs(&manifest, &receipt).map_err(|_| invalid())?;
            canonical_digest(&outputs, manifest.budgets.max_output_bytes.min(16_384))?;
            let summary = outputs["summary"].as_str().ok_or_else(invalid)?;
            if summary.trim().is_empty() || summary.len() > MAX_SUMMARY_BYTES {
                return Err(invalid());
            }
            let prompt = outputs
                .get("prompt")
                .map(|value| {
                    let prompt = value.as_str().ok_or_else(invalid)?;
                    if prompt.len() > MAX_PROMPT_BYTES {
                        return Err(invalid());
                    }
                    Ok(prompt.to_owned())
                })
                .transpose()?
                .filter(|prompt| !prompt.trim().is_empty());
            Ok(ProgramReport {
                summary: summary.to_owned(),
                prompt,
                receipt_digest: algal::runtime::receipt_digest(&receipt).map_err(|_| invalid())?,
            })
        })
        .await
        .map_err(|_| Error::Unavailable("bounded ALGAL program worker failed"))??;
        if *cancel.borrow() {
            return Err(Error::Unavailable("program cancelled; output discarded"));
        }
        if started.elapsed() > MAX_RUN_TIME {
            return Err(Error::Unavailable(
                "program exceeded its reporting deadline",
            ));
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> Value {
        json!({
            "contract":"algal.organism.v1", "key":"organism:project-review", "name":"Project review",
            "cells":[
                {"id":"source","kind":"input","outputs":{"value":"text"}},
                {"id":"report","kind":"fn","fn":"uppercase.v1"},
                {"id":"proposal","kind":"const","outputs":{"value":{"type":"text","value":"Review the next backlog item"}}}
            ],
            "edges":[{"from":{"cell":"source","port":"value"},"to":{"cell":"report","port":"value"}}],
            "interface":{"inputs":{"message":{"cell":"source","port":"value"}},"outputs":{
                "summary":{"cell":"report","port":"value"}, "prompt":{"cell":"proposal","port":"value"}
            }}
        })
    }

    #[tokio::test]
    async fn pinned_pure_program_produces_repeatable_report_and_proposal() {
        let admitted =
            AdmittedProgram::admit(manifest(), json!({"message":"reviewed recent work"})).unwrap();
        let (_sender, cancel) = watch::channel(false);
        let a = admitted.run(cancel.clone()).await.unwrap();
        let b = admitted.run(cancel).await.unwrap();
        assert_eq!(a.summary, "REVIEWED RECENT WORK");
        assert_eq!(a.prompt.as_deref(), Some("Review the next backlog item"));
        assert_eq!(a.receipt_digest, b.receipt_digest);
    }

    #[test]
    fn admission_rejects_effects_unknown_inputs_types_and_excessive_budgets() {
        for kind in [
            "agent", "tool", "store", "load", "organism", "slot", "recall",
        ] {
            let mut source = manifest();
            source["cells"][1]["kind"] = json!(kind);
            assert!(AdmittedProgram::admit(source, json!({"message":"ok"})).is_err());
        }
        assert!(
            AdmittedProgram::admit(manifest(), json!({"message":"ok","extra":"secret"})).is_err()
        );
        assert!(AdmittedProgram::admit(manifest(), json!({"message":12})).is_err());
        assert!(AdmittedProgram::admit(manifest(), json!({})).is_err());
        let mut source = manifest();
        source["budgets"] = json!({"maxAgentCalls":1});
        assert!(AdmittedProgram::admit(source, json!({"message":"ok"})).is_err());
    }

    #[tokio::test]
    async fn tampering_and_preexecution_cancellation_never_produce_a_report() {
        let mut admitted = AdmittedProgram::admit(manifest(), json!({"message":"ok"})).unwrap();
        let (_sender, cancel) = watch::channel(true);
        assert!(admitted.run(cancel).await.is_err());
        admitted.inputs["message"] = json!("changed");
        assert!(admitted.verify().is_err());
    }

    #[tokio::test]
    async fn exhausted_steps_and_oversized_outputs_do_not_settle_as_success() {
        let mut source = manifest();
        source["budgets"] = json!({"maxSteps":1});
        let admitted = AdmittedProgram::admit(source, json!({"message":"ok"})).unwrap();
        let (_sender, cancel) = watch::channel(false);
        assert!(admitted.run(cancel.clone()).await.is_err());
        let admitted = AdmittedProgram::admit(
            manifest(),
            json!({"message":"a".repeat(MAX_SUMMARY_BYTES + 1)}),
        )
        .unwrap();
        assert!(admitted.run(cancel.clone()).await.is_err());
        let mut source = manifest();
        source["budgets"] = json!({"maxOutputBytes":1});
        let admitted = AdmittedProgram::admit(source, json!({"message":"ok"})).unwrap();
        assert!(admitted.run(cancel).await.is_err());
        let mut source = manifest();
        source["budgets"] = json!({"maxContextBytes":1});
        assert!(AdmittedProgram::admit(source, json!({"message":"ok"})).is_err());
    }
}

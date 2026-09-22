//! Provider wire protocols feed one host-owned turn lifecycle. Codecs never
//! execute project tools, settle account leases, or manufacture permissions.
use crate::{Result, process::StreamProcess};
use serde_json::Value;
use xcb_core::{
    models::ModelChoice,
    policy::{Failure, Terminal},
    usage::Counters,
};

pub(crate) struct ImageInput {
    pub media_type: String,
    pub base64: String,
}
pub(crate) struct Prompt {
    pub text: String,
    pub images: Vec<ImageInput>,
}

#[derive(Debug)]
pub(crate) enum Event {
    /// Exact provider/config/tool admission has completed for this turn.
    Ready,
    Delta {
        thinking: bool,
        text: String,
    },
    Assistant(String),
    /// Monotonic cumulative output token observation for this turn.
    OutputTokens(u64),
    Tool {
        id: String,
        name: String,
        arguments: Value,
    },
    /// A request was denied; only a human can supply new permission.
    Attention,
    /// Only host-selected, bounded diagnostic categories cross this boundary.
    Diagnostic(crate::runner::Diagnostic),
    Quota {
        window: Option<String>,
        used_percent: Option<f64>,
        resets_at_ms: Option<u64>,
        failure: Option<Failure>,
    },
    Result {
        terminal: Terminal,
        text: String,
        models: Vec<(String, Counters)>,
    },
    Subagent {
        id: String,
        status: String,
        label: String,
        model: Option<String>,
    },
}

pub(crate) struct Batch {
    pub bytes: usize,
    pub events: Vec<Event>,
}

pub(crate) trait Protocol: Send {
    fn refreshes_catalog(&self) -> bool {
        true
    }

    /// Provider-reported account identity observed during initialization:
    /// `(email, plan)`. Display metadata only — never credential material.
    fn account_identity(&self) -> (Option<String>, Option<String>) {
        (None, None)
    }

    fn next(&mut self, process: &mut StreamProcess) -> impl Future<Output = Result<Batch>> + Send {
        async move {
            let frame = process.frame().await?.ok_or(crate::Error::Protocol(
                "provider ended without a terminal result",
            ))?;
            let bytes = frame.len();
            Ok(Batch {
                bytes,
                events: self.receive(process, &frame).await?,
            })
        }
    }
    /// Listener and callback custody is stopped independently of the dropped
    /// turn future. Return true only after all adapter-owned work has joined.
    fn shutdown(&mut self) -> impl Future<Output = bool> + Send {
        async { true }
    }

    fn initialize(
        &mut self,
        process: &mut StreamProcess,
        instructions: &str,
    ) -> impl Future<Output = Result<Vec<ModelChoice>>> + Send;
    fn start(
        &mut self,
        process: &mut StreamProcess,
        prompt: Prompt,
    ) -> impl Future<Output = Result<()>> + Send;
    /// A bounded frame can produce multiple observations. Discovery replies
    /// may be sent here; executable tool requests must become Event::Tool.
    fn receive(
        &mut self,
        process: &mut StreamProcess,
        frame: &[u8],
    ) -> impl Future<Output = Result<Vec<Event>>> + Send;
    /// Return a result only for an outstanding, uniquely identified tool call.
    fn reply(
        &mut self,
        process: &mut StreamProcess,
        id: &str,
        result: Value,
    ) -> impl Future<Output = Result<()>> + Send;
}

use clap::Subcommand;
use std::path::{Path, PathBuf};
use xcb_core::Id;
use xcb_runtime::{
    Error, Result,
    broker::Workspace,
    context_recipe::{ContextPlan, ContextRecipe, MAX_PLAN_BYTES, MAX_RECIPE_BYTES, read_input},
    managed::ManagedStore,
    managed_program::ProgramCallResult,
    private,
};

#[derive(Subcommand)]
pub enum ContextCommand {
    /// Select exact source excerpts and build an ALGAL inspection program; no model calls.
    Prepare {
        /// JSON question, paths and one to four subquestions.
        plan: PathBuf,
        /// Private output directory, created if absent; recipe.json must not exist.
        #[arg(long)]
        output: PathBuf,
    },
    /// Show source addresses and retrieval measurements, or read one exact chunk.
    Inspect {
        /// Saved recipe.json from context prepare.
        recipe: PathBuf,
        /// Exact chunk digest from the recipe's source index.
        #[arg(long)]
        chunk: Option<String>,
    },
    /// Return the next request for an embedding host; does not dispatch a task.
    Next {
        /// Saved recipe.json from context prepare.
        recipe: PathBuf,
        /// Completed requestDigest and summary records, in call order.
        #[arg(long)]
        results: Option<PathBuf>,
    },
    /// Replay exact recorded subquestion results without calling a provider.
    Replay {
        /// Saved recipe.json from context prepare.
        recipe: PathBuf,
        /// Completed managed program from `xcb backlog context`.
        #[arg(long, required_unless_present = "results", conflicts_with = "results")]
        task: Option<Id>,
        /// JSON array of requestDigest and summary records for offline experiments.
        #[arg(long)]
        results: Option<PathBuf>,
    },
}

pub fn load(path: &Path) -> Result<ContextRecipe> {
    ContextRecipe::parse(&read_input(path, MAX_RECIPE_BYTES)?)
}

pub async fn run(root: &Path, cwd: &Path, command: ContextCommand) -> Result<i32> {
    let value = match command {
        ContextCommand::Prepare { plan, output } => {
            let plan: ContextPlan = serde_json::from_slice(&read_input(&plan, MAX_PLAN_BYTES)?)?;
            plan.validate()?;
            let workspace = Workspace::open(&cwd.canonicalize()?)?;
            let documents = workspace.context_documents(&plan.paths)?;
            let recipe = ContextRecipe::build(plan, documents)?;
            let output = if output.is_absolute() {
                output
            } else {
                std::env::current_dir()?.join(output)
            };
            let output = private::directory(&output)?.join("recipe.json");
            private::create(&output, &serde_json::to_vec(&recipe)?)?;
            serde_json::json!({"recipe":output,"digest":recipe.digest,
                "managedCalls":recipe.program.managed_calls,"providerCalls":0})
        }
        ContextCommand::Inspect { recipe, chunk } => {
            let recipe = load(&recipe)?;
            match chunk {
                Some(address) => recipe.read_chunk(&address)?,
                None => recipe.inspect()?,
            }
        }
        ContextCommand::Next { recipe, results } => {
            let recipe = load(&recipe)?;
            let responses = match results {
                Some(path) => serde_json::from_slice(&read_input(&path, 64 * 1024)?)?,
                None => vec![],
            };
            recipe.advance(responses).await?
        }
        ContextCommand::Replay {
            recipe,
            task,
            results,
        } => {
            let recipe = load(&recipe)?;
            let (responses, receipt): (Vec<ProgramCallResult>, Option<String>) =
                if let Some(task) = task {
                    let store = ManagedStore::open(root)?;
                    let record = store.program_record(&store.resolve_task(&task)?)?;
                    if record["program"] != serde_json::to_value(&recipe.program)? {
                        return Err(Error::Conflict("task does not match context recipe"));
                    }
                    (
                        serde_json::from_value(record["results"].clone())?,
                        Some(
                            record["receiptDigest"]
                                .as_str()
                                .ok_or(Error::Unavailable("task record missing digest"))?
                                .to_owned(),
                        ),
                    )
                } else {
                    let path = results.ok_or(Error::Unavailable(
                        "context replay requires task or results",
                    ))?;
                    (
                        serde_json::from_slice(&read_input(&path, 64 * 1024)?)?,
                        None,
                    )
                };
            let report = recipe.replay(responses).await?;
            if receipt.is_some_and(|digest| report["receiptDigest"] != digest) {
                return Err(Error::Conflict("replayed task record changed"));
            }
            report
        }
    };
    crate::print_json(value)?;
    Ok(0)
}

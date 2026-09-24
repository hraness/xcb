import type { ReactNode } from "react";
import { publishedRelease } from "../publication";
import { readmeHtml } from "../readme.generated";
import type { DocsSlug } from "./topics";

function Code({ children }: { children: string }) {
  return <pre tabIndex={0}><code>{children}</code></pre>;
}

// The README renderer emits these fixed tags and escapes text/code content.
// Add scroll-region semantics without rewriting the generated document's content.
export function accessibleReferenceHtml(html: string): string {
  let tableNumber = 0;
  return html
    .replaceAll("<pre>", '<pre tabindex="0">')
    .replaceAll("<table>", () => `<div class="xcb-docs-table-wrap" role="region" aria-label="Reference table ${++tableNumber}" tabindex="0"><table>`)
    .replaceAll("</table>", "</table></div>");
}

function Note({ children }: { children: ReactNode }) {
  return <div className="xcb-docs-note">{children}</div>;
}

function GettingStarted() {
  return (
    <>
      <Note>Start with the native Rust CLI. {publishedRelease === null
        ? <>No native xcb release or <code>@hraness/xcb</code> npm package is published yet</>
        : <>The latest verified release is v{publishedRelease.version}</>}; the source version is a build identity, not a downloadable release. Check <a href="https://github.com/hraness/xcb/releases">release assets</a> before installing.</Note>
      <h2 id="requirements">Before you start</h2>
      <p>You need Git, Rust <strong>1.97.1</strong>, platform build tools, and a supported provider CLI. Native release binaries are built for macOS ARM64 (<code>darwin-aarch64</code>) and Linux x86_64 (<code>linux-x86_64</code>); other hosts build from source. Claude requires macOS Seatbelt or an admitted Linux <code>bwrap</code> configuration. The Codex and Devin candidates currently require macOS. The isolated command runner is available on macOS ARM64.</p>
      <h2 id="install">1. Build and install</h2>
      <Code>{`git clone https://github.com/hraness/xcb.git
cd xcb
rustup toolchain install 1.97.1 --profile minimal
./scripts/install-native.sh
export PATH="$HOME/.local/bin:$PATH"
xcb --version
xcb --help`}</Code>
      <p>The installer builds with the lockfile and installs <code>~/.local/bin/xcb</code>. Use <code>XCB_INSTALL_PREFIX</code> to choose another prefix. The TypeScript compatibility CLI installs as <code>xcb-compat</code>, so it does not shadow the native command; if an older compatibility install still answers to <code>xcb</code>, <code>command -v xcb</code> shows which binary is active. The installer records its method and keeps a verified helper beside the binary for future upgrades.</p>
      <h2 id="connect">2. Connect Claude</h2>
      <p>Install an admitted Claude Code binary: major version 2, version 2.1.268 or newer. Then create an account and complete the browser sign-in. xcb does not silently import an existing provider login. Replace <code>&lt;account-id&gt;</code> with the generated ID printed by account creation or import; <code>xcb accounts</code> also lists it.</p>
      <Code>{`xcb accounts add claude --plan Max
xcb doctor --provider claude
xcb accounts login <account-id>
xcb accounts refresh <account-id>
xcb models`}</Code>
      <p><code>--plan Max</code> is a display label, not subscription verification. If xcb discovers the wrong executable, select it explicitly with <code>xcb doctor --provider claude --executable /absolute/path/to/claude</code>.</p>
      <p>Using Codex or Devin? Follow the <a href="/docs/providers">provider-specific setup</a> and its exact runtime requirements.</p>
      <h2 id="first-session">3. Open your project</h2>
      <p>Managed tasks select among admitted, available accounts and observed models. Start a task with <code>Use Claude</code>, <code>Use Codex</code>, or <code>Use Devin</code> to require one provider.</p>
      <Code>{`xcb --cwd /absolute/path/to/your/project`}</Code>
      <p>Plain <code>xcb</code> creates a persistent control conversation. Closing it detaches while tasks keep running. Use <code>/tasks</code> to inspect work and <code>/sessions</code> to switch control conversations. Prompts create new work, or answer a task waiting for input; <code>new task: …</code> explicitly starts separate work.</p>
      <p>Ask xcb to explain a file or make a small change. Use <code>/help</code> inside the terminal for interactive commands. To run tests or builds, first <a href="/docs/workspace">set up the isolated command runner</a>.</p>
      <h2 id="updates">4. Keep the global install current</h2>
      <p>Updates are user-level and release-based. xcb defaults to <code>notify</code>; choose <code>auto</code> to let the scheduler install only an exact stable archive with its adjacent SHA-256 checksum. Scheduled checks are macOS-only: a LaunchAgent runs them daily when enabled, and on Linux you run <code>xcb update check</code> from your own timer. Project settings cannot change this policy.</p>
      <Code>{`xcb update check
xcb update enable --policy notify   # macOS only: daily check, no replacement
xcb update enable --policy auto     # macOS only: daily check and verified install
xcb update status
xcb upgrade
xcb update disable`}</Code>
      <p>{publishedRelease === null
        ? "No native xcb release is published yet, so checks fail closed and leave a source install untouched."
        : <>The updater installs the verified v{publishedRelease.version} archive for the running host with its matching checksum.</>} After an upgrade, restart open terminals and rerun <code>xcb doctor</code>; provider and application qualification is tied to the exact version and digest of the installed executable.</p>
      <p>A current managed supervisor detects replacement of its executable, stops starting new turns, and exits after its active workers settle. Queued tasks and tasks waiting for input remain saved. Wait for that exit before refreshing provider pins and reopening the conversation. xcb reports a different running supervisor build explicitly; a legacy supervisor without an identity record must be identified and stopped after its workers settle. Never delete its lock or trust a saved PID alone.</p>
      <p>Older source builds may reject newer managed record fields. Restart old clients and supervisors when upgrading and preserve managed state if rolling back the binary. An installation does not establish fresh live acceptance across all three providers.</p>
      <h2 id="daily-commands">Come back to your work</h2>
      <Code>{`xcb conversations
xcb chat --resume <conversation-id>
xcb tasks
xcb sessions               # direct provider sessions
xcb resume                 # latest direct provider session
xcb resume <session-id>
xcb --cwd /absolute/path/to/your/project run --account <account-id> -p "Explain this repository"
xcb accounts
xcb config`}</Code>
      <p><code>resume</code> reopens the saved session and its workspace in the interactive terminal. It is not a headless continuation command. A JSON run includes the session ID for later use with <code>xcb resume</code>.</p>
      <h2 id="state-and-updates">State and updates</h2>
      <p>Native sessions and credentials live in <code>~/.local/share/xcb</code>. Override the root with <code>--state /absolute/path</code> or <code>XCB_STATE</code>. The <code>xcb-compat</code> compatibility CLI uses <code>~/.xcb</code>; keep their state directories separate.</p>
      <p>After an xcb or provider upgrade, restart open xcb terminals and rerun <code>doctor</code>. A new provider version is not automatically admitted. Keep the source checkout matching your installed CLI for command-runner setup and updates.</p>
    </>
  );
}

function Providers() {
  return (
    <>
      <p>xcb names your accounts, stores their credentials outside the project, and lets you select the account and model for a turn. Replace <code>&lt;account-id&gt;</code> in the setup commands with the generated ID printed by creation or import; names come from observed provider identities, not custom labels. Connecting an account, observing its models, and proving an execution route are separate steps.</p>
      <h2 id="provider-status">Provider status</h2>
      <div className="xcb-docs-table-wrap" role="region" aria-labelledby="provider-status-caption" tabIndex={0}><table>
        <caption id="provider-status-caption">Native CLI evidence and current limits</caption>
        <thead><tr><th scope="col">Provider</th><th scope="col">Current route</th><th scope="col">What has been checked</th></tr></thead>
        <tbody>
          <tr><th scope="row">Claude</th><td>Admitted Claude Code 2.1.268 or newer, major 2.</td><td>Installed coding workflow passed on macOS ARM64 with the tested account. Linux remains a candidate after its host checks.</td></tr>
          <tr><th scope="row">Codex</th><td>Exact admitted 0.155.0-alpha.2.6 build on macOS.</td><td>Authenticated file operations and installed coding workflow passed with the tested account.</td></tr>
          <tr><th scope="row">Devin</th><td>Exact admitted 3000.11.1 and 3000.10.31 builds on macOS.</td><td>Both credential-free boundary fixtures passed. Authenticated coding acceptance requires separate account/model/build evidence; model availability is checked against the account’s fresh catalog at launch.</td></tr>
        </tbody>
      </table></div>
      <p>Codex and Devin admission checks both executable bytes and version. A visible model or <code>metadata pin only</code> from <code>doctor</code> does not prove successful coding on your host. The separate TypeScript CLI keeps Codex and Devin task execution disabled pending qualification.</p>
      <h2 id="claude">Claude</h2>
      <Code>{`xcb accounts add claude --plan Max
xcb doctor --provider claude
xcb accounts login <account-id>
xcb accounts refresh <account-id>
xcb models`}</Code>
      <p>Complete the browser sign-in. Refresh obtains supported model and usage metadata. The plan name is only a label.</p>
      <h2 id="codex">Codex</h2>
      <p>xcb supervises the official CLI’s ChatGPT device sign-in in a private profile.</p>
      <Code>{`xcb doctor --provider codex
xcb accounts add codex --plan ChatGPT
xcb accounts login <account-id>
xcb accounts refresh <account-id>
xcb models`}</Code>
      <p>Alternatively, explicitly import one existing ChatGPT credential:</p>
      <Code>{`xcb accounts import-codex --source /absolute/path/to/auth.json
xcb accounts refresh <account-id>`}</Code>
      <p>The source file stays in place. xcb does not copy provider configuration, plugins, sessions, or transcripts. This route does not accept API-key credentials.</p>
      <h2 id="devin">Devin</h2>
      <p>Sign in through Devin’s CLI first, then select its credential file explicitly.</p>
      <Code>{`devin auth login
xcb doctor --provider devin
xcb accounts import-devin --source /absolute/path/to/credentials.toml
xcb accounts refresh <account-id>
xcb models`}</Code>
      <p>Import preserves the original credentials and sessions. To refresh just the catalog, use <code>xcb models refresh devin --account &lt;account-id&gt;</code>. Native xcb currently supports fixed ACP model choices; compatibility catalog entries for Adaptive or Fusion do not establish native support. A successful <code>devin auth status</code> and populated catalog confirm provider access, not a qualified xcb coding turn. The selected model is checked against the connected account’s fresh catalog before each turn. The September 20, 2026 quota result is historical evidence, not a statement of current availability.</p>
      <h2 id="selection">Select an account and model</h2>
      <p>Copy the matching full key from <code>xcb models</code>. Defaults apply to new direct sessions; a saved session keeps its account binding. Managed tasks route automatically among eligible accounts and models.</p>
      <Code>{`xcb accounts default <account>
xcb models default <full-model-key>
xcb run --account <account> --model <full-model-key> -p "Explain this repository"
xcb accounts disable <account>
xcb accounts enable <account>`}</Code>
      <p>One account can own one active provider turn. Managed tasks in separate workspaces can use separate accounts concurrently. Tasks sharing a workspace run one at a time, and the isolated command backend runs one command at a time. A disabled account remains stored with its history.</p>
      <h2 id="quota">Understand quota information</h2>
      <p>Known Claude account-wide exhaustion stays blocked until the provider’s reported reset. Automatic selection skips those accounts, and an explicit selection explains the block. <code>xcb accounts</code> shows a retry estimate; a reset permits another attempt but does not promise service availability.</p>
      <p>Unknown and stale usage stays unknown. xcb does not infer an account-wide block from arbitrary Codex buckets or Devin exhaustion errors. For exact scopes and credential binding, read the <a href="https://github.com/hraness/xcb/blob/main/docs/quota-routing.md">quota routing contract</a>.</p>
    </>
  );
}

function Workspace() {
  return (
    <>
      <p>Workspace file tools can inspect and change project files through xcb’s broker. Tests and builds use a separate, explicitly provisioned Linux VM. Commands run against a staged copy of the project, without host mounts, provider credentials, or network access.</p>
      <h2 id="setup">Set up the command runner</h2>
      <p>On macOS ARM64, install Lima 2.2 or later at <code>/opt/homebrew/bin/limactl</code> and Python 3. Use the source checkout matching your installed native CLI. The dedicated VM uses an 8 GiB sparse disk, 3 GiB memory, and two CPUs. Setup also enforces an 8 GiB host free-space floor plus provisioning capacity.</p>
      <p>Run from that xcb checkout. Where the HRA host scheduler is installed, use the scheduler-wrapped setup command in the <a href="https://github.com/hraness/xcb/blob/main/docs/command-runner.md#setup-and-admission">command-runner contract</a>.</p>
      <Code>{`/usr/bin/python3 scripts/setup-command-runner.py \\
  --root "$HOME/.local/share/xcb-command" --source "$PWD"`}</Code>
      <p>Setup installs the fixed Linux toolchains and runs the required boundary suite before admitting the backend. It includes Rust 1.97.1, Node 24.18.1, and Bun 1.3.14. When setup succeeds, ask xcb to run your project’s checks.</p>
      <h2 id="dependencies">Prepare public dependencies</h2>
      <p>Ordinary commands are offline. Host <code>node_modules</code>, Cargo outputs, and package-manager credentials are not copied into the VM. For a cold project, first inspect a dependency plan from the same checkout:</p>
      <Code>{`/usr/bin/python3 -I scripts/prepare-command-dependencies.py \\
  --root "$HOME/.local/share/xcb-command" \\
  --workspace /absolute/path/to/project --dry-run`}</Code>
      <p>After reviewing the plan, replace <code>--dry-run</code> with <code>--prepare</code>. Use the installed host scheduler where available, as shown in the <a href="https://github.com/hraness/xcb/blob/main/docs/command-runner.md#dependencies-and-git">full preparation guide</a>.</p>
      <p>Preparation accepts root <code>Cargo.toml</code> + <code>Cargo.lock</code>, <code>package.json</code> + <code>bun.lock</code>, or both. It fetches checksum-bound public dependencies into an immutable cache. Private registries and install scripts are not supported. Changed manifests or lockfiles require a new preparation; a nested project with its own lockfile needs its own workspace.</p>
      <h2 id="limits">Know the boundary</h2>
      <ul>
        <li>Commands run on Linux, offline. Native macOS, Xcode, Simulator, and arbitrary network commands are unavailable.</li>
        <li>Each command has up to 10 minutes, 2 GiB scratch space, and 1.5 GiB worker memory.</li>
        <li>Input snapshots are limited to 64 MiB and 8,192 visited entries. Conventional secret paths and dependency/build directories are excluded; this is not a general secret scanner.</li>
        <li>Git supports filtered, read-only status and diffs. Original history, remotes, hooks, commits, and pushes are unavailable.</li>
        <li>Successful changes are revision-checked before publication. Each file replacement is atomic; the entire batch is not a transaction.</li>
      </ul>
      <p>Read the <a href="https://github.com/hraness/xcb/blob/main/docs/command-runner.md">full command-runner contract</a> for snapshot, output, publication, and cache limits.</p>
      <h2 id="cancel-and-recover">Cancel and recover</h2>
      <p>In managed chat, say <code>cancel &lt;task-id&gt;</code> from any control conversation and inspect <code>/tasks</code> for settlement. For a direct session, cancel in the terminal that owns the turn. For a headless run, Ctrl-C or SIGTERM requests cleanup. xcb must prove the owned processes have stopped before releasing the account; another terminal can view a session without owning its cancellation.</p>
      <Code>{`xcb recover
xcb recover <run-id> --yes`}</Code>
      <p>Inspect the retained run before using <code>--yes</code>. Recovery requires the original host owner to be gone and independently checks pending command receipts. It never publishes staged edits. Do not delete lock files or infer recovery from an elapsed timeout or missing PID.</p>
      <p>For an interrupted dependency preparation, inspect its exact cache key:</p>
      <Code>{`/usr/bin/python3 -I scripts/prepare-command-dependencies.py \\
  --root "$HOME/.local/share/xcb-command" \\
  --status --cache-key CACHE_KEY_FROM_PLAN`}</Code>
      <p>Replace <code>--status</code> with <code>--recover</code> to stop, join, and reconcile that attempt without starting another download.</p>
      <h2 id="refresh">After a backend change or VM restart</h2>
      <p>Admission binds the VM’s boot identity and exact tool bytes. Stop active commands and repeat setup with <code>--refresh</code> when fresh admission is required. Keep the backend and installed CLI matched, and restart open terminals after replacing xcb. Refresh does not clear an unsettled job; resolve that job through recovery first.</p>
    </>
  );
}

function Customization() {
  return (
    <>
      <p>xcb keeps presentation, optional extensions, and execution authority separate. Inspect your current setup before changing it:</p>
      <Code>{`xcb config
xcb panes
xcb plugins`}</Code>
      <h2 id="sessions">Move between sessions</h2>
      <p>In managed chat, <code>/sessions</code> switches control conversations, <code>/tasks</code> inspects the shared task swarm, and <code>/new</code> starts a new task prompt. To reopen a saved direct provider session, use <code>xcb resume</code>. In that direct terminal, <code>/sessions</code> selects provider sessions, <code>/new</code> starts another, and <code>/accounts</code> and <code>/model</code> open account and model pickers. Finish the current turn before changing its account or model.</p>
      <h2 id="panes">Arrange your terminal</h2>
      <p>In a direct provider terminal, open the pane picker with <code>/pane</code>, switch to the built-in focused view with <code>/pane focus</code>, or edit the current declaration with <code>/pane edit</code>. To describe a new view, use <code>/pane generate &lt;description&gt;</code>, replacing the placeholder with what you want to see. Generation uses the selected account and model; if that session is busy, it waits for the account’s next idle boundary.</p>
      <p>Panes describe what the terminal displays. They are bounded presentation data, not executable plugins, and cannot grant a provider new tools or permissions.</p>
      <Code>{`xcb panes show focus
xcb panes check /absolute/path/to/pane.json
xcb panes install /absolute/path/to/pane.json`}</Code>
      <p>Use the built-in pane as a starting point and validate a declaration before installing it. The terminal’s <code>/help</code> lists the interactive controls.</p>
      <h2 id="continuation">Continuation and context</h2>
      <p>Auto-continue and Gobstopper context management are enabled by default. Continuation is bounded and requires a safely settled turn. Context management reduces retained context according to the configured policy. To continue turns that ended before the work was done, see <a href="/docs/reflexes#continuation">learned continuation</a>.</p>
      <Code>{`xcb plugins disable auto-continue
xcb plugins disable gobstopper
# Enable them again:
xcb plugins enable auto-continue
xcb plugins enable gobstopper`}</Code>
      <p>One provider turn has a 30-minute default deadline, including initialization. The private state root’s <code>config.json</code> accepts <code>turn_timeout_ms</code> from 1,000 to 3,600,000 milliseconds. <code>xcb config</code> displays the effective settings; continuation has its own separate limits.</p>
      <h2 id="usage">Local usage and explicit opt-ins</h2>
      <p>Usage measurement stays local. Automatic aiCharts upload is unavailable. Local exports, executable hooks, and external judgment require separate opt-in; enabling a presentation pane cannot enable them.</p>
      <p>Hooks execute code and need their own trust decision. Review the <a href="/docs/reference#optional-behavior">optional behavior reference</a> and <code>xcb hooks --help</code> before adding one.</p>
      <h2 id="judge">An optional external judge</h2>
      <p>The TypeSafe System One judge can advise routing, continuation, and context retention. It sends bounded task and response context to that service; it cannot qualify a provider or bypass deterministic checks. It is disabled by default.</p>
      <Code>{`xcb judge token < /secure/path/to/judge-key
xcb judge status
xcb judge enable
xcb judge test
# To stop using the judge:
xcb judge disable
xcb judge logout`}</Code>
      <p>The input is an existing private credential file. Keys are stored outside the workspace; do not put a key in a command-line argument. See the <a href="/docs/reference#optional-behavior">complete reference</a> for environment and custom-endpoint behavior.</p>
    </>
  );
}

function Reflexes() {
  return (
    <>
      <Note>Auto-certification ships in v0.6.0, with settle and confirm defaulting to <code>auto</code> and observing until certified. Earlier v0.5.0 downloads default both to <code>observe</code>, require explicit <code>active</code> settings for them to act, and do not accept <code>auto</code>. See the <a href="https://github.com/hraness/xcb/blob/v0.5.0/docs/reflexes.md">v0.5.0 reference</a> for those binaries.</Note>
      <p>A reflex is a small decision xcb makes many times a day and can learn from how you respond. Two ship today: <strong>route</strong> picks the frontier or standard model tier for a new task, and <strong>settle</strong> categorizes how a worker&apos;s turn ended, including whether it stopped before the task was done.</p>
      <Code>{`xcb reflex                 # status of both reflexes
xcb reflex status settle   # generation, live precision and recall, open trials`}</Code>
      <h2 id="shape">How a decision is made</h2>
      <p>Each decision is a small ALGAL program applied to deterministic features, learned parameters, and typed evidence. The program has no effects and makes no model calls, so every decision is a replayable receipt. Parameters are data: learning adds a generation and never edits the program.</p>
      <div className="xcb-docs-table-wrap" role="region" aria-label="Reflexes" tabIndex={0}><table>
        <thead><tr><th scope="col">Reflex</th><th scope="col">Reads</th><th scope="col">Decides</th><th scope="col">v0.6.0 default</th></tr></thead>
        <tbody>
          <tr><th scope="row">route</th><td>Prompt shape (imperative opening, resume language, action verbs, length), keyword cues, and the optional judge&apos;s answers</td><td>frontier or standard</td><td>active</td></tr>
          <tr><th scope="row">settle</th><td>How much work the turn did (tool calls), the end of the worker&apos;s report (in progress, waiting on CI, asking for a go-ahead, handing you a step, naming a risky action, a structured final summary) and the turn&apos;s settlement facts</td><td>done, stopped short, confirm, question, needs approval, blocked, interrupted, …</td><td>auto</td></tr>
        </tbody>
      </table></div>
      <p>Route&apos;s shipped parameters reproduce xcb&apos;s behavior before reflexes. Settle&apos;s are fitted on 2,428 real follow-up messages and tuned for precision: about four in five turns it calls stopped short were followed by &ldquo;continue&rdquo;, and about two in three it calls confirm were followed by &ldquo;yes&rdquo;.</p>
      <h2 id="learning">How it learns from you</h2>
      <ul>
        <li>Reply <code>continue</code>, <code>keep going</code>, or <code>push it</code> right after a task completes, and xcb records that the turn stopped short. With settle on <code>active</code> or <code>auto</code>, it also reopens that task in its session.</li>
        <li>Reply <code>yes</code> or <code>go ahead</code> and xcb records that the turn was waiting for your confirmation.</li>
        <li>Start something else, correct the worker, or say a handoff is done, and the turn is recorded as neither, at half weight.</li>
        <li>When settle continues a turn for you, the continuation labels itself: real work confirms the decision, and cancelling it counts against it.</li>
        <li>Ask for a stronger or lighter model (&ldquo;use opus&rdquo;, &ldquo;cheaper model&rdquo;) and the previous task&apos;s route is labeled.</li>
        <li>Label anything explicitly. Explicit labels always win over inferred ones.</li>
      </ul>
      <Code>{`xcb reflex label route t_… frontier
xcb reflex label settle t_… unfinished
xcb reflex train settle`}</Code>
      <p>Every 16 labels, xcb fits a challenger anchored on the shipped prior. The challenger then runs a forward trial: it and the active generation are scored on the next 48 labels, which neither was fitted on. It replaces the active generation only if it lowers log loss there without losing accuracy or ranking quality. No label is held out from learning forever, and every generation records its parent and the trial that promoted it.</p>
      <h2 id="continuation">Continuing work that stopped short</h2>
      <p>When the <code>settle</code> head acts (always under <code>active</code>, and under <code>auto</code> once certified), a completed turn categorized as stopped short is continued in its existing session with a prompt to carry out the step it described. The same gates as any automatic continuation apply first: a joined and settled worker, no pending question or approval, no failure or uncertain effect, a response that is not a repeat, and remaining attempt and time budget. A configured judge can still veto it, and a turn held out for you is never continued by the judge.</p>
      <p>A turn categorized as confirm (&ldquo;Should I open the PR and merge it?&rdquo;) is answered &ldquo;yes, go ahead&rdquo; only when the <code>confirm</code> head acts too, and never while settle only observes. xcb never answers a request whose report mentions deleting, dropping, deploying, releasing, production, spending, credentials, or sending something, or that hands a step to you. That check is in the runtime, so a replaced program cannot remove it. A vetoed request is never answered; if its turn was cut off by a limit it may still continue with the generic prompt, and a judge can only veto that.</p>
      <Note>The v0.6.0 release defaults settle and confirm to auto mode. Categories appear on tasks and xcb learns from your replies. A head acts only once a replay of your own replies certifies its precision: at least 0.75 for continuing a stopped-short turn and 0.85 for answering a go-ahead, as a 99% lower bound. It returns to observing if that precision falls. About one acting turn in ten is still left for you, so the evidence stays honest.</Note>
      <h2 id="configure">Configure, roll back, or replace</h2>
      <Code>{`# v0.6.0 defaults: config.json → extensions.reflexes
{ "route": "active", "settle": "auto", "confirm": "auto", "learn": true }

xcb reflex rollback route 0          # back to the shipped prior
xcb reflex import settle history.jsonl --dry-run   # replay your history first
xcb reflex check my-route.algal.json`}</Code>
      <p>Each reflex can be <code>off</code>, <code>observe</code>, <code>active</code>, or (settle and confirm) <code>auto</code>. <code>xcb reflex status settle</code> shows each head&apos;s certificate. To change the decision logic itself, put an organism at <code>reflexes/route.algal.json</code> or <code>reflexes/settle.algal.json</code> in the state directory. xcb admits it only if it has no effectful cells and no agent calls; otherwise status reports the rejection and the shipped program runs.</p>
      <p>The ledger stores numeric features, decisions, and labels, never prompt or response text. A learned route predicts the tier you would pick, not measured model quality. It only chooses between routes that are already eligible and cannot demote the quality floor for large prompts. See the <a href="https://github.com/hraness/xcb/blob/main/docs/reflexes.md">reflex reference</a> for the full contract.</p>
    </>
  );
}

function ApplicationApi() {
  return (
    <>
      <p>Let xcb own provider sign-in and process custody while your application owns its data and actions. The application interface accepts a bounded prompt and returns untrusted text. It runs with no tools, hooks, session history, continuation, or account fallback.</p>
      <h2 id="discover">Discover qualified routes</h2>
      <Code>{`xcb --json generate --capabilities`}</Code>
      <p>This reads local metadata without provider refresh or inference. Select only an account with <code>available: true</code> and one of its exact model keys. <code>supported: true</code> alone does not mean an account is available.</p>
      <p>Capabilities verifies local executable bytes and can take several seconds. Give discovery a bounded timeout separate from the generation deadline; 90 seconds is a practical desktop integration recommendation, not a protocol timing guarantee.</p>
      <p>When launching xcb from an app, preserve <code>HOME</code> or set <code>XCB_STATE</code> in the child environment, even when passing <code>--state</code>; the current CLI still resolves its default root. Close unused stdin for discovery and drain stdout and stderr while waiting for the subprocess to finish.</p>
      <Note>Application qualification is separate from coding support. A fresh installation reports <code>supported: false</code> until the exact xcb executable, provider, account, and model have current evidence. A successful sign-in or <code>doctor</code> check is not enough. Do not substitute <code>xcb run</code> when generation is unavailable.</Note>
      <h2 id="generate">Generate one response</h2>
      <p>Start the admitted executable directly with <code>--json generate</code>. Write one UTF-8 JSON document to stdin, close stdin, and drain stdout and stderr while waiting for the result. Keep both streams bounded. Replace both account and model placeholders with the exact values from capabilities.</p>
      <Code>{`{
  "version": 1,
  "account": "<available-account-id>",
  "model": "<qualified-full-model-key>",
  "prompt": "Summarize the supplied text in one sentence.",
  "timeoutMs": 60000,
  "maxOutputBytes": 65536
}`}</Code>
      <p>All six fields are required; additional fields are rejected. The total input is limited to 1 MiB. The timeout range is 1,000–120,000 milliseconds and the output limit is 1–262,144 bytes. Prompts must be nonempty and contain no NUL.</p>
      <p>A successful response has <code>status: completed</code>, generated <code>text</code>, and an outcome proving the provider has joined with no application effects. A nonzero exit returns a closed failure object without generated text. Validate the text against your own application schema before acting on it.</p>
      <h2 id="host-responsibilities">Keep application actions in your host</h2>
      <p>xcb performs inference, not your application’s file access, network requests, or message delivery. Your host controls recipient selection, authorization, output validation, and durable request records. <a href="https://github.com/hraness/textbutler">TextButler</a> is a reference consumer that keeps contact access and messaging approvals in its own host.</p>
      <p>Send SIGINT or SIGTERM to request cancellation, then wait for cleanup. The inference deadline does not prove that the provider has stopped. An uncertain result keeps account custody and must not be blindly retried.</p>
      <h2 id="qualification">Qualification and renewal</h2>
      <p>Qualification uses actual host boundary evidence and a separate fixed live challenge. Receipts expire no later than 24 hours after evidence collection begins. Runtime, provider, or explicit credential changes can invalidate them earlier. Reading capabilities never extends their lifetime.</p>
      <p>Use the <a href="https://github.com/hraness/xcb/blob/main/qualification/README.md#application-qualification-prerequisites">host qualification procedure</a> before accepting application traffic. The explicit macOS <a href="https://github.com/hraness/xcb/blob/main/docs/application-renewal.md">Claude renewal helper</a> binds one previously qualified deployment and account; it is not activated by installing xcb.</p>
      <p>For the complete discovery and response schemas, error codes, expiry rules, and custody contract, read the <a href="https://github.com/hraness/xcb/blob/main/docs/application-api.md">application API reference</a>.</p>
    </>
  );
}

function RouteTasks() {
  return (
    <>
      <Note><code>xcb --json route</code> is the machine contract for another program — typically a coding agent — to hand XCB one task and get back one settled, routed turn. For bounded tool-free text generation instead, see the <a href="/docs/application-api">application API</a>.</Note>
      <h2 id="contract">The route contract</h2>
      <p>Write one UTF-8 JSON document to stdin, close stdin, and read a bounded JSON document from stdout. Unknown fields are rejected; every field except <code>version</code>, <code>workspace</code>, and <code>task</code> is optional.</p>
      <Code>{`$ xcb --json route
{
  "version": 1,
  "workspace": "/absolute/path/to/project",
  "task": "Fix the failing parser test and show the diff",
  "provider": "claude",            // optional pin
  "account": "<account-id>",       // optional pin
  "model": "claude/sonnet/low",    // optional pin, exact observed key
  "timeoutMs": 1800000,            // optional caller deadline
  "dryRun": false                  // true selects a route without running
}`}</Code>
      <p><code>provider</code>, <code>account</code>, and <code>model</code> are eligibility constraints, not fallbacks. A <code>provider</code> that disagrees with the pinned account&apos;s provider is rejected. With no pins, XCB selects among admitted runtimes, credentialed enabled accounts that are idle and outside known quota windows, and observed fresh model entries — ranked by task class and relative quality, cost, and latency Pareto tiers, with an optional judge ordering only already-eligible routes.</p>
      <p>A completed call returns <code>status: &quot;completed&quot;</code> only for a completed, joined, settled turn with no pending attention, and includes the selected <code>route</code>, the saved <code>session</code> id (reopen with <code>xcb resume</code>), the recorded <code>outcome</code> facts, and bounded <code>text</code>.</p>
      <h2 id="failures">Failure codes</h2>
      <p>Failures exit nonzero with a closed object: <code>invalid_request</code>, <code>unavailable</code> (no eligible route, unknown account or model), <code>busy</code> (account custody held by live work), <code>deadline</code> (the caller&apos;s <code>timeoutMs</code> expired), <code>cancelled</code>, <code>provider_error</code> (including quota failures — <code>outcome.failure</code> carries the exact detail), <code>custody_unproven</code> (process exit could not be proven; do not retry blindly), and <code>needs_input</code> (the provider stopped with a question — <code>text</code> carries it, and the saved session can be resumed by a person).</p>
      <p>When a request provably launched no provider process, the failure carries <code>joined: true</code> and <code>effects: &quot;none&quot;</code>. SIGINT and SIGTERM request the same bounded cancellation and settlement path as <code>timeoutMs</code>; killing XCB does not prove the provider stopped.</p>
      <h2 id="sdk">Embedding with the SDK</h2>
      <p>The TypeScript compatibility source exports <code>createSubscriptionRouter</code>, which bundles an account lease store and qualified task adapters into one object. It is a source build — no <code>@hraness/xcb</code> package is published — and there is no bundled live adapter; the host supplies adapters and qualification evidence.</p>
      <Code>{`import { openAccountDatabase, SqliteAccountLeases, createSubscriptionRouter } from "@hraness/xcb";

const db = await openAccountDatabase("/private/path/to/router.sqlite");
const router = createSubscriptionRouter({
  leases: new SqliteAccountLeases(db),
  adapters: [claudeTaskAdapter],
});

const result = await router.run({
  provider: "claude",
  accountId: "a_…",
  profile: { id: profile.id, version: profile.version, digest: profile.digest },
  model: { id: "claude-sonnet-5", reasoningEffort: "low", serviceTier: null },
  purpose: "respond",
  prompt: "Summarize the diff in this workspace.",
  limits: { maxRunMs: 60_000, maxCleanupMs: 10_000, maxOutputBytes: 65_536 },
}, broker);`}</Code>
      <p><code>router.routes()</code> lists the adapter-registered routes a caller can dispatch to — registration is not eligibility. <code>router.run(request, broker)</code> accepts provider-plus-authentication shorthand when exactly one adapter matches, defaults <code>runId</code>/<code>workspaceId</code> to the broker&apos;s binding, and still performs every admission, lease, deadline, and stop-evidence check.</p>
      <p>For the complete schema, custody semantics, and selection rules, read the <a href="https://github.com/hraness/xcb/blob/main/docs/route.md">route contract</a> and <a href="https://github.com/hraness/xcb/blob/main/docs/quota-routing.md">quota routing</a> references in the repository.</p>
    </>
  );
}

export function TopicContent({ slug }: { slug: DocsSlug }) {
  switch (slug) {
    case "route": return <RouteTasks />;
    case "getting-started": return <GettingStarted />;
    case "providers": return <Providers />;
    case "workspace": return <Workspace />;
    case "customization": return <Customization />;
    case "reflexes": return <Reflexes />;
    case "application-api": return <ApplicationApi />;
    case "reference": return <><p className="xcb-docs-eyebrow">Repository reference</p><p>This page is generated from the current <a href="https://github.com/hraness/xcb/blob/main/README.md">repository README</a>. For a shorter path, start with the <a href="/docs/getting-started">setup guide</a>.</p><div dangerouslySetInnerHTML={{ __html: accessibleReferenceHtml(readmeHtml) }} /></>;
  }
}

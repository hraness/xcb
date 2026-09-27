import type { ReactNode } from "react";
import { ProviderMark } from "@hraness/design-kit/react/server";
import { publishedRelease } from "../publication";
import { providerStatus, supportedBuilds } from "./provider-status";
import { sdkExample, sdkExampleOutput } from "./sdk-example";
import type { DocsSlug } from "./topics";

const repository = "https://github.com/hraness/xcb";
const repositoryDocs = `${repository}/blob/main/docs`;

function Code({ children }: { children: string }) {
  return <pre tabIndex={0}><code>{children}</code></pre>;
}

function Note({ children }: { children: ReactNode }) {
  return <div className="xcb-docs-note">{children}</div>;
}

/** A link that leaves xcb.sh. */
function Ext({ href, children }: { href: string; children: ReactNode }) {
  return <a href={href}>{children}<span aria-hidden="true"> ↗</span></a>;
}

function Table({ label, head, rows }: { label: string; head: readonly string[]; rows: readonly (readonly ReactNode[])[] }) {
  return (
    <div className="xcb-docs-table-wrap" role="region" aria-label={label} tabIndex={0}>
      <table>
        <thead><tr>{head.map((cell) => <th scope="col" key={cell}>{cell}</th>)}</tr></thead>
        <tbody>
          {rows.map((row, index) => (
            <tr key={index}>{row.map((cell, column) => column === 0 ? <th scope="row" key={column}>{cell}</th> : <td key={column}>{cell}</td>)}</tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

const releaseVersion = publishedRelease?.version ?? null;
const devinBuilds = supportedBuilds.devin.join(", ");

function GettingStarted() {
  return (
    <>
      <p>This tutorial installs xcb, connects one Claude account, and sends a task to a small practice project. At the end you’ll see which account and model xcb picked and the fix it made.</p>
      <h2 id="requirements">Before you start</h2>
      <ul>
        <li>A Mac with Apple silicon or a Linux x86_64 machine. Other hosts can <a href="#build-from-source">build from source</a>.</li>
        <li>A Claude subscription and Claude Code {supportedBuilds.claudeMinimum} or later. Check with <code>claude --version</code>.</li>
        <li>On Linux, Claude runs only after you <a href="/docs/providers#claude-on-linux">run xcb’s sandbox checks</a> on that machine. Do that before step 2.</li>
      </ul>
      <h2 id="install">1. Install xcb</h2>
      {releaseVersion === null
        ? <p>No verified release is published yet, so <a href="#build-from-source">build from source</a>.</p>
        : <>
          <p>The installer downloads the v{releaseVersion} release for your platform, checks its SHA-256 checksum, and installs <code>~/.local/bin/xcb</code>:</p>
          <Code>{`curl -fsSL https://xcb.sh/install.sh | sh
xcb --version`}</Code>
          <p>If your shell can’t find <code>xcb</code>, add <code>export PATH=&quot;$HOME/.local/bin:$PATH&quot;</code> to your shell profile and open a new terminal. The <a href="/install">install page</a> covers the other options.</p>
        </>}
      <h2 id="connect">2. Connect Claude</h2>
      <Code>{`xcb setup claude`}</Code>
      <p><code>xcb setup</code> adds an account, checks your Claude Code build, opens the browser sign-in, and loads the account’s models. It ends with “Claude Code is set up.” xcb keeps this sign-in in its own state folder, so your usual Claude Code login is unaffected.</p>
      <h2 id="practice-project">3. Make a practice project</h2>
      <Code>{`mkdir -p ~/xcb-tutorial && cd ~/xcb-tutorial
git init -q
printf 'export function add(a, b) {\\n  return a - b;\\n}\\n' > add.js`}</Code>
      <p><code>add()</code> subtracts. Making the folder a Git repository lets xcb treat it as a project when you start xcb there.</p>
      <h2 id="preview">4. Preview the route</h2>
      <Code>{`xcb models route --task "Fix add() in add.js so it adds"`}</Code>
      <p>The first line names the model key and the account ID xcb would use right now, then the reasons. Nothing runs and no account is held.</p>
      <h2 id="first-task">5. Send the task from your thread</h2>
      <Code>{`xcb`}</Code>
      <p>This opens your thread. Type <code>Fix add() in ~/xcb-tutorial/add.js so it adds instead of subtracting.</code> and press Enter. xcb replies with the folder it chose and why, such as “Started <strong>Fix add()</strong> in <code>xcb-tutorial</code> · path in <code>xcb-tutorial</code> · /workspace to move”. The session card above the chat shows the routed model while the task runs, and the answer appears in the thread when it finishes.</p>
      <p>Check the result from another terminal:</p>
      <Code>{`cat ~/xcb-tutorial/add.js`}</Code>
      <p>The function now returns <code>a + b</code>. xcb routed the task to one of your accounts, ran Claude Code in a sandbox with access to this folder only, and recorded how the run ended.</p>
      <h2 id="come-back">6. Close the terminal and come back</h2>
      <p>Send a second task, such as <code>Add a test for add()</code>, and close the terminal window while it runs. The task keeps going. Run <code>xcb</code> again and type <code>/tasks</code> to see it finished.</p>
      <p>To run one task without the thread, use <code>xcb run -p &quot;Explain this repository&quot;</code> in a project folder. It prints the answer when the task finishes.</p>
      <h2 id="build-from-source">Build from source</h2>
      <p>On an Intel Mac, ARM Linux, or any host without a release archive, build with Git, Rust 1.97.1, and the platform’s build tools:</p>
      <Code>{`git clone https://github.com/hraness/xcb.git
cd xcb
rustup toolchain install 1.97.1 --profile minimal
./scripts/install-native.sh
export PATH="$HOME/.local/bin:$PATH"`}</Code>
      <p>The installer builds with the lockfile, installs <code>~/.local/bin/xcb</code>, and warns when another <code>xcb</code> earlier on your <code>PATH</code> would shadow it.</p>
      <h2 id="next">Next steps</h2>
      <ul>
        <li><a href="/docs/projects-and-tasks">Projects and tasks</a>: work across several projects from one thread.</li>
        <li><a href="/docs/providers">Accounts and models</a>: connect Codex and Devin, and choose a model yourself.</li>
        <li><a href="/docs/workspace">Tests and builds</a>: let tasks run your project’s checks.</li>
        <li><a href="/docs/route">Route tasks</a>: send work from another agent or a script.</li>
      </ul>
    </>
  );
}

function HowRoutingWorks() {
  return (
    <>
      <p>Every task goes through the same four steps, whether you type it in the thread, run <code>xcb run</code>, or send it with <code>xcb --json route</code>: xcb filters your accounts, ranks the models they offer, holds the chosen account while the provider works, and records how the run ended.</p>
      <h2 id="filter">1. Find the accounts that can take the task</h2>
      <p>An account can take a task when all of these are true:</p>
      <ul>
        <li>Its provider build is one xcb supports (see <a href="/docs/providers#supported-builds">supported builds</a>).</li>
        <li>It is signed in and enabled. An account whose sign-in failed stays out until you sign in again.</li>
        <li>It is idle: no other task is using it.</li>
        <li>It is not at a known usage limit. When Claude reports 100% use of its five-hour or seven-day window, xcb skips that account until the reset the provider reported. Unknown usage stays unknown and does not block the account.</li>
        <li>The model was seen recently in the provider’s model list.</li>
      </ul>
      <p>Your constraints narrow the list first. A prompt that starts with <code>Use Claude</code>, <code>Use Codex</code>, or <code>Use Devin</code> requires that provider, and <code>xcb run --account</code> or a pinned <code>provider</code>, <code>account</code>, or <code>model</code> in a route request does the same. xcb never falls back outside a constraint.</p>
      <h2 id="rank">2. Rank the models</h2>
      <p>xcb gives each model relative quality, cost, and latency scores and sorts the models into tiers: a model is in the first tier when no other model beats it on all three at once. It then scores the tiers for routine, balanced, or complex work.</p>
      <p>The kind of task comes from the <a href="/docs/reflexes">route reflex</a>, a small classifier that learns from the model tiers you ask for. If you turn on the <a href="/docs/security#judge">optional judge</a>, its answers feed the same classifier. A prompt of at least 400 words or 8 KiB always gets the highest-quality model available, whatever its price. Remaining usage, your <code>favorites</code> in <a href="/docs/reference#configuration">config.json</a>, and the provider you usually pick for that project break ties.</p>
      <p>The scores are heuristics, not measured quality or prices. When a usage limit rules out a stronger model, the chosen route says so.</p>
      <h2 id="hold">3. Hold the account</h2>
      <p>Before the provider starts, xcb holds the account so no other task can use it. Each account runs one provider turn at a time, and tasks in the same project folder, or in a folder inside it, take turns. Tasks in different folders on different accounts run at the same time.</p>
      <p>The provider runs in an operating-system sandbox: Seatbelt on macOS, <code>bwrap</code> on Linux. It gets xcb’s file tools for the task’s folder, no shell of its own, and network access to port 443 only. See <a href="/docs/security">security and privacy</a> for what that covers.</p>
      <h2 id="record">4. Record how the run ended</h2>
      <p>When the provider process exits, xcb records the result: completed, needs your input, failed, or cancelled. If it can’t confirm that the provider stopped or what it changed, it records the run as uncertain, keeps the account held, and does not retry. <code>xcb recover</code> shows those runs; see <a href="/docs/troubleshooting#unfinished-run">troubleshooting</a>.</p>
      <p>In the thread, a turn that stops at a usage limit continues on another route the task hasn’t tried yet, and a turn cut off by a turn or token limit can continue on its own (up to three times in a row within ten minutes by default). An uncertain run never moves to another account. <code>xcb --json route</code> runs exactly one turn and leaves retries to the caller.</p>
      <h2 id="preview">Preview a decision</h2>
      <Code>{`xcb models route --task "fix a race in the scheduler"   # the route xcb would pick here, now
xcb models tiers --task "fix a race in the scheduler"   # every model's tier and scores`}</Code>
      <p>Neither command holds an account, and the choice can change before a task runs. A route request with <code>&quot;dryRun&quot;: true</code> reports the same decision as JSON.</p>
      <Note>The TypeScript SDK does not rank or choose: your app names the account and model, and the SDK holds that account while the task runs. See <a href="/docs/sdk">TypeScript SDK</a>.</Note>
    </>
  );
}

function Security() {
  return (
    <>
      <p>xcb runs on your machine. Your accounts, credentials, and task history stay there, and model requests go from each provider’s own CLI to that provider.</p>
      <h2 id="local">What stays on your machine</h2>
      <ul>
        <li><strong>State:</strong> accounts, credentials, conversations, tasks, and settings live in <code>~/.local/share/xcb</code> (or <code>XCB_STATE</code>), readable only by you.</li>
        <li><strong>Credentials:</strong> xcb stores a sign-in only when you run <code>xcb setup</code>, <code>xcb accounts login</code>, or an explicit <code>import</code> command. It never reads your existing provider logins on its own, and import copies the one file you name, leaving it in place.</li>
        <li><strong>Usage:</strong> token and usage measurement stays local. Uploading usage to AI Charts is unavailable.</li>
        <li><strong>Learning:</strong> the <a href="/docs/reflexes">reflexes</a> store numeric features and your labels, never prompt or response text.</li>
      </ul>
      <h2 id="providers">What goes to providers</h2>
      <p>The provider CLI sends your prompt, and any file contents the model reads through xcb’s tools, to its own service under your account. Each provider’s terms and data policies apply.</p>
      <p>While a task runs, the provider process is sandboxed: Seatbelt on macOS, <code>bwrap</code> on Linux. It runs a private copy of the provider executable xcb checked, uses a configuration folder xcb created for that account, reaches the network on port 443 only, and reaches your project only through xcb’s file tools, which stay inside the task’s folder. Credentials never enter a project folder. The <a href="/docs/workspace">command runner</a> gives commands a copy of the project with no credentials and no network.</p>
      <h2 id="judge">The optional judge</h2>
      <p>The judge is off by default. When you enable it, xcb sends questions to TypeSafe’s System One service (<code>api.typesafe.ai</code>) to help classify tasks, decide whether a turn stopped short, and choose which old tool output to keep. Each request carries at most 128 KiB:</p>
      <ul>
        <li>routing: up to 8 KiB of the task text;</li>
        <li>continuation: up to 8 KiB each of the original task and the last response;</li>
        <li>context management: up to 88 KiB of recent messages, plus the names and sizes of old tool results, never their contents.</li>
      </ul>
      <p>The judge can only order routes that already passed every check. It cannot add a provider or skip a safety check. Its key is stored in the state folder with mode 0600, or read from <code>XCB_JEV_API_KEY</code>. See <a href="/docs/customization#judge">customize xcb</a> to turn it on.</p>
      <h2 id="network">Other network requests</h2>
      <ul>
        <li><strong>Supported builds:</strong> about once an hour, xcb reads the list of reviewed provider builds from this repository’s <code>qualified-builds.json</code> on GitHub.</li>
        <li><strong>Offers:</strong> the background supervisor reads the public Devin pricing page when it starts and every six hours.</li>
        <li><strong>Updates:</strong> <code>xcb update check</code>, <code>xcb upgrade</code>, and the optional daily check read release data from GitHub.</li>
        <li><strong>Remote devices:</strong> only after <code>xcb link</code>. Task content crosses the relay end-to-end encrypted.</li>
      </ul>
      <p>None of these send prompts, files, or account details.</p>
      <h2 id="extensions">Extensions that run code</h2>
      <p>Hooks run programs you choose at session and turn events. They are off by default and need their own <code>xcb plugins enable hooks</code>. Panes change only what the terminal shows; they cannot run code or grant a provider new tools.</p>
      <h2 id="report">Report a vulnerability</h2>
      <p>Report security issues privately as described in the <Ext href={`${repository}/blob/main/SECURITY.md`}>security policy</Ext>.</p>
    </>
  );
}

function ProjectsAndTasks() {
  return (
    <>
      <p>Plain <code>xcb</code> opens your thread from any directory: one conversation for all your projects. Each prompt becomes a task in one project folder, and tasks keep running after you close the terminal.</p>
      <h2 id="choose-folder">How xcb picks a task’s folder</h2>
      <p>A project is a folder. For each prompt in the thread, xcb uses the first of these that applies:</p>
      <ol>
        <li>a path in the prompt, such as <code>in ~/src/app</code>;</li>
        <li>a registered project name in the prompt;</li>
        <li>the project you focused with <code>/workspace</code>;</li>
        <li>the project of your last task, when the prompt reads as a continuation;</li>
        <li>the folder you started xcb from (<code>--cwd</code> sets it), then recent work.</li>
      </ol>
      <p>The reply names the folder and the reason, such as “Started <strong>Fix the parser</strong> in <code>app</code> · named <code>app</code> · /workspace to move”. When a prompt names a folder xcb hasn’t seen, or matches several projects, xcb asks, keeps your draft, and saves nothing. A less certain choice waits 8 seconds before it starts so you can move it.</p>
      <Code>{`xcb --cwd /absolute/path/to/your/project   # the thread, hinting this project
xcb chat --new                              # a project view: every task runs in this directory`}</Code>
      <p>xcb picks only folders you have used or registered, and never your home folder, its hidden folders, <code>~/Library</code>, or system folders. Register one with <code>/workspace add &lt;dir&gt;</code> or <code>xcb workspaces add &lt;dir&gt;</code>. The <Ext href={`${repositoryDocs}/managed-harness.md#choosing-a-tasks-directory`}>managed harness guide</Ext> lists every rule.</p>
      <h2 id="follow">Follow your tasks</h2>
      <ul>
        <li><code>/tasks</code> lists running and finished work; <code>xcb tasks</code> does the same from a script.</li>
        <li><code>/attention</code> collects questions and approvals from every task. Answer a question with <code>/reply &lt;task-id&gt; &lt;answer&gt;</code>.</li>
        <li>The session cards above the chat show each task’s model and latest response. Press F6 to browse them.</li>
        <li>Open another terminal for a second view of the same tasks.</li>
      </ul>
      <h2 id="guide">Guide or stop a task</h2>
      <ul>
        <li><code>/steer &lt;task-id&gt; &lt;guidance&gt;</code> adds guidance for the task’s next turn without interrupting it. In <code>/agents</code>, select a task and press <code>s</code> to write guidance or <code>a</code> to answer its question; <code>/task</code> returns to new work.</li>
        <li><code>/cancel &lt;task-id&gt;</code> asks a task to stop. With several running tasks and none selected, a bare <code>/cancel</code> asks which one.</li>
        <li><code>/workspace move &lt;task&gt; &lt;name|path&gt;</code> moves a task that hasn’t started. A started task can’t move; cancel it and send the prompt again.</li>
      </ul>
      <p>Cancellation finishes when the provider has stopped and xcb has recorded the result.</p>
      <h2 id="resume">Come back to your work</h2>
      <Code>{`xcb                                   # your thread, from any directory
xcb conversations                     # the thread and project views
xcb chat --resume <conversation-id>   # reopen one of them
xcb workspaces                        # project folders the thread picks from
xcb attention                         # questions and approvals waiting on you`}</Code>
      <p>Inside xcb, <code>/sessions</code> switches between the thread and project views. In the thread, <code>/new</code> clears the project focus; in a project view it starts another view.</p>
      <h2 id="direct-sessions">Direct provider sessions</h2>
      <p><code>xcb run</code> and <code>xcb --json route</code> save direct sessions, each on one account and model.</p>
      <Code>{`xcb sessions                 # direct provider sessions
xcb resume                   # reopen the latest one in the terminal
xcb resume <session-id>`}</Code>
      <p><code>xcb resume</code> reopens a session and its folder in the terminal. It is not a headless continuation command. In a direct session, <code>/accounts</code> and <code>/model</code> open the account and model pickers, <code>/new</code> starts another session, and <code>/sessions</code> switches between them. Finish the current turn before changing its account or model.</p>
      <h2 id="project-work">Backlog, schedules, and project agents</h2>
      <p>Each project keeps a backlog and work history shared by the thread and every project view over its folder.</p>
      <ul>
        <li><code>/backlog add &lt;work&gt;</code> saves work for later; <code>/backlog</code> shows this project’s queue and <code>/backlog all</code> every project’s.</li>
        <li><code>/schedule every &lt;seconds&gt; &lt;prompt&gt;</code> repeats a prompt. Schedules run while the background supervisor runs; on macOS, <code>xcb service install</code> starts it at login.</li>
        <li><code>/project grant &lt;tasks&gt; &lt;hours&gt; &lt;goal&gt;</code> lets a project start a limited number of follow-up tasks on its own; <code>/project pause</code> stops that. A grant covers only its own folder.</li>
      </ul>
      <p>These commands never guess a project: without one you named, the focus, or a selected task, they ask and save nothing. The <Ext href={`${repositoryDocs}/project-agents.md`}>project agents reference</Ext> covers programs, daemons, and Wordcell memory, and the <Ext href={`${repositoryDocs}/terminal.md`}>terminal guide</Ext> covers keys, search, Vim editing, and draft recovery.</p>
    </>
  );
}

function Providers() {
  return (
    <>
      <p>xcb names each account after the identity the provider reports, stores its credentials in xcb’s state folder, and checks the provider’s executable before running it. Replace <code>&lt;account-id&gt;</code> below with the ID xcb prints when it adds the account; <code>xcb accounts</code> lists them.</p>
      <h2 id="supported-builds">Supported builds</h2>
      <div className="xcb-docs-table-wrap" role="region" aria-labelledby="provider-status-caption" tabIndex={0}><table>
        <caption id="provider-status-caption">Provider builds xcb runs, and their status</caption>
        <thead><tr><th scope="col">Provider</th><th scope="col">Supported builds</th><th scope="col">Status</th></tr></thead>
        <tbody>
          <tr><th scope="row">Claude</th><td>Claude Code {supportedBuilds.claudeMinimum} or later within version 2, on macOS or Linux</td><td>{providerStatus.claude} On Linux, Claude needs <a href="#claude-on-linux">xcb’s sandbox checks</a> first.</td></tr>
          <tr><th scope="row">Codex</th><td>Codex CLI {supportedBuilds.codex.join(", ")} on macOS ARM64</td><td>{providerStatus.codex}</td></tr>
          <tr><th scope="row">Devin</th><td>Devin CLI {devinBuilds} on macOS ARM64</td><td>{providerStatus.devin}</td></tr>
        </tbody>
      </table></div>
      <p>For Codex and Devin, xcb checks the executable’s SHA-256 as well as its version. When a provider updates itself to a build xcb hasn’t reviewed, <code>xcb doctor</code> says it is waiting for review and xcb keeps using the build it already checked. Reviewed builds are published in the repository’s <Ext href={`${repository}/blob/main/qualified-builds.json`}>qualified-builds.json</Ext>, and xcb picks them up within an hour without an upgrade.</p>
      <h2 id="claude" className="xcb-provider-heading"><ProviderMark mark="claudecode" label="Claude Code" size={24} />Claude</h2>
      <Code>{`xcb setup claude`}</Code>
      <p>Setup runs these steps, which you can also run one at a time:</p>
      <Code>{`xcb accounts add claude --plan Max
xcb doctor --provider claude
xcb accounts login <account-id>
xcb accounts refresh <account-id>
xcb models`}</Code>
      <p><code>--plan</code> is a label shown in <code>xcb accounts</code>; xcb doesn’t check it against your subscription. If xcb finds the wrong Claude Code, point it at the right one with <code>xcb doctor --provider claude --executable /absolute/path/to/claude</code>. xcb keeps a private copy of that executable, so a Claude Code auto-update can’t change it mid-task.</p>
      <h3 id="claude-on-linux">Claude on Linux</h3>
      <p>On Linux, xcb runs Claude inside <code>bwrap</code> only after checks on that machine prove the sandbox works. You need <code>bwrap</code> at <code>/usr/bin/bwrap</code>, unprivileged user namespaces (on Ubuntu 24.04, <code>sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0</code>), a source checkout, and Bun 1.3.14. From the checkout:</p>
      <Code>{`BWRAP="$(command -v bwrap)"
SHA="$(sha256sum "$BWRAP" | cut -d' ' -f1)"
mkdir -p evidence
for probe in linux-sandbox linux-egress linux-loopback; do
  code=0
  bun "qualification/$probe.ts" --bwrap "$BWRAP" --bwrap-sha256 "$SHA" > "evidence/$probe.json" || code=$?
  echo "$code" > "evidence/$probe.exitcode"
done
bun qualification/build-receipt.ts "$BWRAP" "$SHA" evidence/linux-qualification.json
install -D -m 600 evidence/linux-qualification.json ~/.local/share/xcb/qualification/linux.json`}</Code>
      <p>The result is valid for 30 days, for that <code>bwrap</code> binary and those namespace settings. On Linux, <code>xcb doctor</code> shows whether the sandbox is ready.</p>
      <h2 id="codex" className="xcb-provider-heading"><ProviderMark mark="codex" label="Codex" size={24} />Codex</h2>
      <Code>{`xcb setup codex`}</Code>
      <p>Setup supervises the Codex CLI’s ChatGPT device sign-in in a private profile; follow the code shown in the terminal. To reuse a ChatGPT sign-in you already have, copy its <code>auth.json</code> into a new xcb account instead:</p>
      <Code>{`xcb accounts import-codex --source /absolute/path/to/auth.json
xcb accounts refresh <account-id>`}</Code>
      <p>Import leaves the source file in place and copies no Codex settings, plugins, or sessions. API-key sign-ins aren’t accepted. xcb runs Codex with the gpt-6-astra and gpt-5.6-sol models.</p>
      <h2 id="devin" className="xcb-provider-heading"><ProviderMark mark="devin" label="Devin" size={24} />Devin</h2>
      <p>Sign in with the Devin CLI, then copy that sign-in into xcb:</p>
      <Code>{`devin auth login
xcb doctor --provider devin
xcb accounts import-devin --source /absolute/path/to/credentials.toml
xcb accounts refresh <account-id>
xcb models`}</Code>
      <p>Import leaves the Devin CLI’s credentials and sessions in place. To connect with a token instead, pipe it into <code>xcb accounts token &lt;account-id&gt;</code>. To refresh only the model list, run <code>xcb models refresh devin --account &lt;account-id&gt;</code>. xcb checks the chosen model against the account’s current model list before each Devin turn.</p>
      <h2 id="choose">Choose an account or model</h2>
      <p>You don’t need to pick: the thread and <code>xcb run</code> choose automatically. To choose yourself:</p>
      <Code>{`xcb run --account <account> --model <full-model-key> -p "Explain this repository"
xcb accounts default <account>      # the default for new direct sessions
xcb models default <full-model-key> # a preferred model; a stronger route can still win
xcb accounts disable <account>      # keep it, but route nothing to it
xcb accounts enable <account>`}</Code>
      <p>Copy full model keys, such as <code>claude/sonnet/low</code>, from <code>xcb models</code>. A saved session keeps its account. In the thread, start a prompt with <code>Use Claude</code>, <code>Use Codex</code>, or <code>Use Devin</code> to require that provider.</p>
      <h2 id="usage-limits">Usage limits</h2>
      <p><code>xcb accounts</code> shows each account’s known usage and when it resets. When Claude reports an account-wide limit, xcb routes around that account until the reported reset and says why if you pick it yourself. A reset allows another attempt; it doesn’t promise the provider will accept it. Usage xcb hasn’t measured stays unknown, and xcb doesn’t guess a limit from Codex or Devin errors. <code>xcb --json accounts</code> adds <code>quotaBlockedUntilMs</code>. The <Ext href={`${repositoryDocs}/quota-routing.md`}>quota routing reference</Ext> has the details.</p>
    </>
  );
}

function Workspace() {
  return (
    <>
      <p>xcb’s file tools let a task read and change project files. To run tests and builds, set up the command runner: a Linux VM on your Mac that runs each command against a copy of the project, with no network, no host folders, and no credentials.</p>
      <Note>The command runner works on macOS ARM64 only. It runs Linux commands, so native macOS and Xcode builds can’t run, and Git inside it is read-only.</Note>
      <h2 id="setup">Set up the runner</h2>
      <p>You need Lima 2.2 or later at <code>/opt/homebrew/bin/limactl</code> (<code>brew install lima</code>), Python 3, 8 GiB of free disk space plus room for the VM, and a checkout of the xcb source at the same version as your installed <code>xcb</code>. Setup reads the runner’s files from that checkout, so use the source checkout matching your installed native CLI:</p>
      <Code>{`${releaseVersion === null ? "git clone https://github.com/hraness/xcb.git" : `git clone --depth 1 --branch v${releaseVersion} https://github.com/hraness/xcb.git`}
cd xcb
/usr/bin/python3 scripts/setup-command-runner.py \\
  --root "$HOME/.local/share/xcb-command" --source "$PWD"`}</Code>
      <p>Setup creates a dedicated VM (8 GiB sparse disk, 3 GiB memory, two CPUs), installs Rust 1.97.1, Node 24.18.1, Bun 1.3.14, Python, Git, and a C compiler inside it, and runs a test suite before xcb will use it. Then ask a task to run your project’s checks.</p>
      <h2 id="dependencies">Prepare public dependencies</h2>
      <p>Commands run offline, and your host’s <code>node_modules</code> and Cargo output aren’t copied in. For a project with <code>Cargo.toml</code> and <code>Cargo.lock</code>, <code>package.json</code> and <code>bun.lock</code>, or both, first review the plan from the same checkout:</p>
      <Code>{`/usr/bin/python3 -I scripts/prepare-command-dependencies.py \\
  --root "$HOME/.local/share/xcb-command" \\
  --workspace /absolute/path/to/project --dry-run`}</Code>
      <p>Then replace <code>--dry-run</code> with <code>--prepare</code> to download the checksummed public packages into a read-only cache. Private registries and install scripts aren’t supported. Prepare again after the manifests or lockfiles change; a nested project with its own lockfile is prepared separately.</p>
      <p>If a preparation is interrupted, inspect it with the cache key the plan printed:</p>
      <Code>{`/usr/bin/python3 -I scripts/prepare-command-dependencies.py \\
  --root "$HOME/.local/share/xcb-command" \\
  --status --cache-key CACHE_KEY_FROM_PLAN`}</Code>
      <p>Replace <code>--status</code> with <code>--recover</code> to stop and reconcile that attempt without downloading again.</p>
      <h2 id="limits">Limits</h2>
      <ul>
        <li>Each command gets up to 10 minutes, 2 GiB of scratch space, and 1.5 GiB of memory. The runner runs one command at a time.</li>
        <li>The project copy is limited to 64 MiB and 8,192 entries. Secret files such as <code>.env</code> and <code>.ssh</code>, dependency folders, and build output are left out; this is a path rule, not a secret scanner.</li>
        <li>Git shows status and diffs only. History, remotes, hooks, commits, and pushes are unavailable.</li>
        <li>A command’s file changes are applied only when it succeeds, after checking each file hasn’t changed since the copy. Each file replacement is atomic; the entire batch is not a transaction.</li>
      </ul>
      <p>The <Ext href={`${repositoryDocs}/command-runner.md`}>command runner reference</Ext> lists every limit.</p>
      <h2 id="refresh">After an upgrade or VM restart</h2>
      <p>The runner is bound to the exact files it tested. After you upgrade xcb, stop running commands, check out the matching source version, and run setup again with <code>--refresh</code>. Refresh doesn’t clear a command whose result is uncertain; resolve it with <code>xcb recover</code> first, and don’t delete lock files or job records. <code>xcb command prune --yes</code> archives old finished jobs.</p>
    </>
  );
}

function Customization() {
  return (
    <>
      <p>Settings live in <code>config.json</code> in xcb’s state folder. <code>xcb config</code> prints the settings in effect, and the <a href="/docs/reference#configuration">configuration reference</a> lists every key.</p>
      <Code>{`xcb config
xcb panes
xcb plugins`}</Code>
      <h2 id="panes">Arrange your terminal</h2>
      <p>In a direct provider session, <code>/pane</code> opens the pane picker, <code>/pane focus</code> switches to the built-in focused view, and <code>/pane edit</code> edits the current one. To describe a new view, use <code>/pane generate &lt;description&gt;</code>. Generation uses the selected account and model; if that session is busy, it waits for the account’s next idle boundary.</p>
      <Code>{`xcb panes show focus
xcb panes check /absolute/path/to/pane.json
xcb panes install /absolute/path/to/pane.json`}</Code>
      <p>Panes describe what the terminal shows. They can’t run code or give a provider new tools.</p>
      <h2 id="continuation">Continuation and context</h2>
      <p>Two extensions are on by default. Auto-continue resumes a turn that a turn or token limit cut off, up to three times in a row within ten minutes, only after the turn ended cleanly with no question pending. Gobstopper context management trims old tool output from long sessions and keeps the originals in your local history.</p>
      <Code>{`xcb plugins disable auto-continue
xcb plugins disable gobstopper
# Turn them back on:
xcb plugins enable auto-continue
xcb plugins enable gobstopper`}</Code>
      <p>To continue turns that ended before the work was done, see <a href="/docs/reflexes#continuation">learned continuation</a>.</p>
      <h2 id="deadline">Turn deadline</h2>
      <p>One provider turn, including start-up, has a 30-minute deadline. Set <code>turn_timeout_ms</code> in <code>config.json</code> from 1,000 to 3,600,000 milliseconds to change it. You can cancel a turn before its deadline.</p>
      <h2 id="hooks">Hooks</h2>
      <p>Hooks run a program you choose at <code>session_start</code>, <code>session_end</code>, <code>turn_start</code>, or <code>turn_end</code>, with a 5-second default timeout. They run code, so they are off until you turn them on:</p>
      <Code>{`xcb plugins enable hooks
xcb hooks add turn_end /absolute/path/to/program`}</Code>
      <h2 id="judge">The optional judge</h2>
      <p>The judge asks TypeSafe’s System One model to help classify tasks, decide whether a turn stopped short, and pick which old tool output to keep. It is off by default and needs a TypeSafe key. <a href="/docs/security#judge">Security and privacy</a> lists what it sends.</p>
      <Code>{`xcb judge token < /secure/path/to/judge-key
xcb judge enable
xcb judge test
# To stop using it:
xcb judge disable
xcb judge logout`}</Code>
      <p>Pipe the key from a file; don’t put it in a command-line argument. <code>XCB_JEV_API_KEY</code> or <code>TYPESAFE_API_KEY</code> can supply it instead, and a custom endpoint in <code>XCB_JEV_URL</code> requires an environment key.</p>
    </>
  );
}

function Reflexes() {
  return (
    <>
      <p>A reflex is a small decision xcb makes many times a day and learns from how you respond. Two ship: <strong>route</strong> picks the frontier or standard model tier for a new task, and <strong>settle</strong> sorts how a worker’s turn ended, including whether it stopped before the task was done.</p>
      <Code>{`xcb reflex                 # status of both reflexes
xcb reflex status settle   # generation, live precision and recall, open trials`}</Code>
      <h2 id="shape">How a decision is made</h2>
      <p>Each decision is a small ALGAL program applied to numeric features of the task, learned parameters, and the optional judge’s answers. The program makes no model calls and has no side effects, so xcb can replay any decision. Learning adds a new set of parameters and never edits the program.</p>
      <Table label="Reflexes" head={["Reflex", "Reads", "Decides", "Default"]} rows={[
        ["route", "Prompt shape (imperative opening, resume language, action verbs, length), keyword cues, and the optional judge’s answers", "frontier or standard", "active"],
        ["settle", "How much work the turn did (tool calls), how the worker’s report ends (in progress, waiting on CI, asking for a go-ahead, handing you a step, naming a risky action, a final summary), and how the turn ended", "done, stopped short, confirm, question, needs approval, blocked, interrupted, …", "auto"],
      ]} />
      <p>Route’s shipped parameters reproduce xcb’s behavior before reflexes. Settle’s were fitted on 2,428 real follow-up messages and tuned for precision: about four in five turns it calls stopped short were followed by “continue”, and about two in three it calls confirm were followed by “yes”.</p>
      <h2 id="learning">How it learns from you</h2>
      <ul>
        <li>Reply <code>continue</code>, <code>keep going</code>, or <code>push it</code> right after a task completes, and xcb records that the turn stopped short. When settle acts, it also reopens that task in its session.</li>
        <li>Reply <code>yes</code> or <code>go ahead</code>, and xcb records that the turn was waiting for your confirmation.</li>
        <li>Start something else, correct the worker, or say a handoff is done, and the turn counts as neither, at half weight.</li>
        <li>When settle continues a turn for you, real work in the continuation confirms the decision, and cancelling it counts against it.</li>
        <li>Ask for a stronger or lighter model (“use opus”, “cheaper model”), and the previous task’s route is labeled.</li>
        <li>Label anything yourself. Your labels always win over inferred ones.</li>
      </ul>
      <Code>{`xcb reflex label route t_… frontier
xcb reflex label settle t_… unfinished
xcb reflex train settle`}</Code>
      <p>Every 16 labels, xcb fits a challenger from the shipped starting point. The challenger and the current parameters are then scored on the next 48 labels, which neither was fitted on. The challenger replaces the current parameters only if it lowers log loss there without losing accuracy or ranking quality. Every generation records its parent and the trial it won.</p>
      <h2 id="continuation">Continuing work that stopped short</h2>
      <p>When the settle reflex acts, a completed turn it sorts as stopped short continues in its existing session with a prompt to carry out the step it described. The checks for any automatic continuation apply first: the worker has stopped and its effects are recorded, no question or approval is pending, nothing failed or is uncertain, the response isn’t a repeat, and attempts and time remain. A configured judge can still veto it.</p>
      <p>A turn sorted as confirm (“Should I open the PR and merge it?”) is answered “yes, go ahead” only when the <code>confirm</code> head acts too. xcb never answers a request whose report mentions deleting, dropping, deploying, releasing, production, spending, credentials, or sending something, or that hands a step to you. That check is in the runtime, so a replaced program can’t remove it.</p>
      <p>By default, settle and confirm use <code>auto</code>: sorted categories appear on tasks and xcb learns from your replies, but a head acts only after a replay of your own replies shows its precision is at least 0.75 for continuing a stopped turn and 0.85 for answering a go-ahead, as a 99% lower bound. It goes back to observing if precision falls, and about one acting turn in ten is still left for you so the measurement stays current.</p>
      <h2 id="configure">Configure, roll back, or replace</h2>
      <Code>{`# Defaults in config.json → extensions.reflexes
{ "route": "active", "settle": "auto", "confirm": "auto", "learn": true }

xcb reflex rollback route 0                         # back to the shipped parameters
xcb reflex import settle history.jsonl --dry-run    # replay your history first
xcb reflex check my-route.algal.json`}</Code>
      <p>Each reflex can be <code>off</code>, <code>observe</code>, <code>active</code>, or, for settle and confirm, <code>auto</code>. To change the decision logic itself, put a program at <code>reflexes/route.algal.json</code> or <code>reflexes/settle.algal.json</code> in the state folder. xcb uses it only if it has no side effects and makes no agent calls; otherwise <code>xcb reflex status</code> reports why and the shipped program runs.</p>
      <p>The learning record stores numeric features, decisions, and labels, never prompt or response text. A learned route predicts the tier you would pick, not measured model quality, and it only chooses among routes that can already take the task. The <Ext href={`${repositoryDocs}/reflexes.md`}>reflex reference</Ext> has the full rules.</p>
    </>
  );
}

function UpgradeAndUninstall() {
  return (
    <>
      <p>xcb updates only to verified releases: an exact archive for your platform with a matching SHA-256 checksum, never a branch or an unverified build.</p>
      <h2 id="upgrade">Upgrade</h2>
      <Code>{`xcb update check        # is a newer release available?
xcb upgrade             # install the latest verified release
xcb upgrade <version>   # install a specific release`}</Code>
      <p><code>xcb upgrade</code> reruns the installer that <code>install-native.sh</code> recorded, so it needs an install made with that script. If you built from source, update the checkout and run <code>./scripts/install-native.sh</code> again. The binary being replaced is kept as <code>~/.local/bin/xcb.previous.&lt;sha256&gt;</code>.</p>
      <h2 id="automatic-checks">Automatic checks</h2>
      <p>On macOS, xcb can check once a day with a login item:</p>
      <Code>{`xcb update enable --policy notify   # record a newer release (the default policy)
xcb update enable --policy auto     # also install it
xcb update status                   # the policy, the last check, and any newer release
xcb update disable`}</Code>
      <p>Scheduled checks are macOS-only. On Linux, run <code>xcb update check</code> yourself or from your own user timer.</p>
      <h2 id="after-upgrade">After an upgrade</h2>
      <ol>
        <li>Restart open xcb terminals. A terminal started from the old binary keeps using it.</li>
        <li>Run <code>xcb doctor</code>. Provider checks carry over while the provider executables are unchanged.</li>
        <li>Let the background supervisor finish. The old supervisor starts no new turns, lets running tasks finish, and exits; queued tasks and tasks waiting for you stay saved. Until it exits, xcb says another xcb build owns the supervisor. On macOS, <code>xcb service status</code> shows “supervisor idle” once it has exited.</li>
        <li>If you use the command runner, <a href="/docs/workspace#refresh">refresh it</a> from the matching source version.</li>
      </ol>
      <p>If you’re upgrading from an 0.8 release, the first newer build to open your state moves project grants and Wordcell bindings from conversations to folders. Follow the <Ext href={`${repositoryDocs}/project-agents.md#upgrading-from-an-08-release`}>upgrade steps</Ext> first.</p>
      <p>To go back to an earlier release, run <code>xcb upgrade &lt;version&gt;</code>. An older build may refuse state written by a newer one, so keep your state folder and read the <Ext href={`${repository}/blob/main/CHANGELOG.md`}>changelog</Ext> first.</p>
      <h2 id="uninstall">Uninstall</h2>
      <p>Let running tasks finish or cancel them, then remove the login items and the binary:</p>
      <Code>{`xcb service uninstall   # macOS: stop starting the supervisor at login
xcb update disable      # stop the daily update check
rm ~/.local/bin/xcb`}</Code>
      <p>Your accounts, credentials, and history stay in <code>~/.local/share/xcb</code> until you delete that folder. The table lists everything xcb creates.</p>
      <h2 id="manual-removal">What xcb creates</h2>
      <p>Paths assume the default <code>~/.local</code> prefix.</p>
      <Table label="Files xcb creates" head={["What", "Where", "Notes"]} rows={[
        ["Binary", <code key="b">~/.local/bin/xcb</code>, <>Also <code>~/.local/bin/xcb.previous.*</code> backups.</>],
        ["Installer record", <code key="i">~/.local/share/xcb/install.json</code>, <>With <code>install-native.sh</code> beside it, inside the state folder.</>],
        ["State folder", <code key="s">~/.local/share/xcb</code>, <>Accounts, credentials, conversations, tasks, settings. Or the folder in <code>XCB_STATE</code>.</>],
        ["Update check (macOS)", <code key="u">~/Library/LaunchAgents/dev.hraness.xcb.update.plist</code>, <>Run <code>xcb update disable</code> first, or unload it with <code>launchctl bootout</code>.</>],
        ["Login service (macOS)", <code key="h">~/Library/LaunchAgents/dev.hraness.xcb.habitat.*.plist</code>, <>Run <code>xcb service uninstall</code> while idle. Its log is in <code>~/Library/Logs/xcb</code>.</>],
        ["Command runner", <code key="c">~/.local/share/xcb-command</code>, <>Stop the VM first: <code>LIMA_HOME=~/.local/share/xcb-command/lima limactl stop worker</code>.</>],
        ["Write locks", <code key="w">~/.local/share/xcb-coordination</code>, <>Or the folder in <code>XCB_COORDINATION_ROOT</code>. Remove it only when no xcb process is running.</>],
        ["PATH line", <><code>~/.zprofile</code>, <code>~/.bash_profile</code>, or <code>~/.profile</code></>, <>Only if you installed with <code>XCB_ADD_PATH=yes</code>.</>],
      ]} />
      <p>The TypeScript compatibility CLI, <code>xcb-compat</code>, keeps its own state in <code>~/.xcb</code>. Removing xcb doesn’t sign you out of Claude Code, Codex, or Devin themselves.</p>
    </>
  );
}

function Troubleshooting() {
  return (
    <>
      <p>Start with <code>xcb doctor</code>. It checks each provider build, the list of reviewed builds, and runs that didn’t finish cleanly, then names the next command to run.</p>
      <h2 id="not-found">The shell can’t find xcb, or runs the wrong one</h2>
      <p>Add <code>~/.local/bin</code> to your <code>PATH</code> (<code>export PATH=&quot;$HOME/.local/bin:$PATH&quot;</code>), then check <code>command -v xcb</code>. The installer warns when another <code>xcb</code> earlier on your <code>PATH</code>, such as an old copy, would run instead. The TypeScript compatibility CLI installs as <code>xcb-compat</code>.</p>
      <h2 id="unsupported-build">doctor says a provider build can’t run</h2>
      <p>“found, but xcb can’t run this build yet” means the installed provider isn’t a <a href="/docs/providers#supported-builds">supported build</a>. “waiting for review before xcb runs it” means the provider updated itself; xcb keeps using the build it already checked until the new one is reviewed. Install a supported build, or wait for the review.</p>
      <h2 id="wrong-binary">xcb found the wrong provider executable</h2>
      <Code>{`xcb doctor --provider claude --executable /absolute/path/to/claude`}</Code>
      <p>xcb keeps a private copy of the executable you name and uses it until you pin another.</p>
      <h2 id="sign-in-again">An account needs signing in again</h2>
      <p>After a sign-in fails, xcb stops routing to that account, including after a restart, and <code>xcb accounts</code> marks it. Sign in again:</p>
      <Code>{`xcb accounts login <account>`}</Code>
      <p>For Devin, run <code>devin auth login</code> and import the credentials again. Refreshing the model list or re-importing the same credentials doesn’t clear the mark.</p>
      <h2 id="no-route">No account can take a task</h2>
      <p>A task waits, or a route request fails with <code>unavailable</code>, when no account passes the <a href="/docs/how-routing-works#filter">routing checks</a>. Check <code>xcb accounts</code> for usage limits, disabled accounts, and accounts that need signing in; <code>xcb models</code> for models; and <code>xcb models route --task &quot;…&quot;</code> for the route xcb would pick.</p>
      <h2 id="empty-result">A task finished with no reply and no changes</h2>
      <p>Send it again on another provider by starting the prompt with <code>Use Claude</code> or <code>Use Codex</code>, or pin <code>provider</code> in a route request. To keep tasks off one account for a while, run <code>xcb accounts disable &lt;account&gt;</code>.</p>
      <h2 id="unfinished-run">A run didn’t finish cleanly</h2>
      <p>When xcb can’t confirm that a provider stopped, it keeps that account held and doesn’t retry. <code>xcb doctor</code> reports the run.</p>
      <Code>{`xcb recover                  # list runs that didn't finish cleanly
xcb recover <run-id>         # show what recovery would do
xcb recover <run-id> --yes   # confirm the process is gone and release the account`}</Code>
      <p>Recovery checks that the provider’s processes are gone before it releases the account, and it never applies unpublished command changes. Do not delete lock files or state to get an account back; a missing process ID or an elapsed timer doesn’t prove the provider stopped.</p>
      <h2 id="supervisor">After an upgrade, xcb says another build owns the supervisor</h2>
      <p>The previous build’s supervisor is finishing its running tasks. Wait for it to exit (on macOS, <code>xcb service status</code> shows “supervisor idle”), restart your terminals, and open <code>xcb</code> again. Queued tasks stay saved. See <a href="/docs/upgrade-and-uninstall#after-upgrade">after an upgrade</a>.</p>
      <h2 id="files-and-folders">macOS blocks project folders</h2>
      <p>When the login service runs xcb in the background, macOS may block it from folders such as Documents, Desktop, or Downloads. <code>xcb service status</code> then reports “xcb can’t open files in ~/Documents: macOS access is off for xcb.” Turn on xcb under that folder in System Settings › Privacy &amp; Security › Files &amp; Folders, or open the pane directly:</p>
      <Code>{`open 'x-apple.systempreferences:com.apple.preference.security?Privacy_FilesAndFolders'`}</Code>
      <h2 id="linux-sandbox">Claude won’t start on Linux</h2>
      <p>“native OS confinement is not qualified on this platform” means the sandbox checks haven’t run on this machine, are older than 30 days, or no longer match its <code>bwrap</code> or namespace settings. Run the <a href="/docs/providers#claude-on-linux">Linux sandbox checks</a> again.</p>
      <h2 id="config">config.json is rejected</h2>
      <p>“config.json is incompatible with this xcb build” means the file has a key or value this build doesn’t accept, often after going back to an older release. Fix the key, or remove it to use the default; the <a href="/docs/reference#configuration">configuration reference</a> lists valid values.</p>
      <h2 id="help">Get help</h2>
      <p>Search or open an issue on <Ext href={`${repository}/issues`}>GitHub</Ext>, and include the output of <code>xcb --version</code> and <code>xcb doctor</code>. Report security problems privately through the <Ext href={`${repository}/blob/main/SECURITY.md`}>security policy</Ext>.</p>
    </>
  );
}

function RouteTasks() {
  return (
    <>
      <p><code>xcb --json route</code> lets another program, usually a coding agent, hand xcb one task. xcb picks an account and model that can take it, runs one turn in the project folder you name, and prints one JSON result. For text generation with no tools, use the <a href="/docs/application-api">application API</a> instead.</p>
      <h2 id="request">Send a request</h2>
      <p>Write one UTF-8 JSON document to standard input, close it, and read the result from standard output. Only <code>version</code>, <code>workspace</code>, and <code>task</code> are required, and unknown fields are rejected:</p>
      <Code>{`$ xcb --json route <<'EOF'
{
  "version": 1,
  "workspace": "/absolute/path/to/project",
  "task": "Fix the failing parser test and show the diff",
  "provider": "claude",
  "account": "<account-id>",
  "model": "claude/sonnet/low",
  "timeoutMs": 1800000,
  "dryRun": false
}
EOF`}</Code>
      <ul>
        <li><code>version</code> is <code>1</code>. Pin it; a new request format ships under a new version.</li>
        <li><code>workspace</code> is an existing folder. The provider’s file tools stay inside it.</li>
        <li><code>provider</code> (<code>claude</code>, <code>codex</code>, or <code>devin</code>), <code>account</code> (an ID or exact name), and <code>model</code> (a full key from <code>xcb models</code>) are optional limits on the choice, not fallbacks. A <code>provider</code> that doesn’t match the pinned account is rejected.</li>
        <li><code>timeoutMs</code> is 1,000 to 3,600,000. When it expires, xcb cancels the turn and answers after the provider has stopped.</li>
        <li><code>dryRun: true</code> reports the route without holding an account or starting a provider.</li>
      </ul>
      <p>The request is limited to 1 MiB, and <code>task</code> to 256 KiB. With no pins, xcb chooses the way <a href="/docs/how-routing-works">routing works</a> everywhere else.</p>
      <h2 id="response">Read the result</h2>
      <p>A dry run returns <code>status: &quot;selected&quot;</code> and the route. A run returns <code>status: &quot;completed&quot;</code> only when the turn finished, the provider has exited, and nothing is waiting for an answer:</p>
      <Code>{`{
  "version": 1,
  "status": "completed",
  "requestId": "route_…",
  "session": "s_…",
  "route": { "provider": "claude", "account": "a_…", "model": "claude/sonnet/low", "label": "Sonnet · low", "reason": "…" },
  "state": "idle",
  "outcome": { "terminal": "completed", "joined": true, "effects": "settled", "pending_attention": false, "failure": null },
  "text": "…"
}`}</Code>
      <p><code>text</code> holds up to 256 KiB of the provider’s answer; <code>textTruncated: true</code> marks a longer one. <code>session</code> is a saved direct session a person can reopen with <code>xcb resume &lt;session&gt;</code>. <code>reason</code> is a short description of why xcb chose the route, not a price or quality guarantee. Top-level fields are camelCase; the fields inside <code>outcome</code> are snake_case.</p>
      <h2 id="failures">Handle failures</h2>
      <p>A failure exits 1 with <code>status: &quot;failed&quot;</code> and one <code>code</code>:</p>
      <Table label="Route failure codes" head={["Code", "Meaning", "What to do"]} rows={[
        [<code key="c">invalid_request</code>, "The request is malformed, the folder doesn’t exist, or stdin is a terminal.", "Fix the request."],
        [<code key="c">unavailable</code>, "No account can take the task, or the pinned account or model is unknown.", <>Check <code>xcb --json accounts</code> and <code>xcb models</code>.</>],
        [<code key="c">busy</code>, "The pinned account is running another task.", "Try later or pin another account."],
        [<code key="c">deadline</code>, <>Your <code>timeoutMs</code> expired and the turn was cancelled.</>, "Retry with more time."],
        [<code key="c">cancelled</code>, "The turn was cancelled by SIGINT or SIGTERM.", "Retry if you still want the result."],
        [<code key="c">provider_error</code>, <>The turn failed or hit a usage limit; <code>outcome.failure</code> says which, such as <code>account_quota</code>.</>, "Send it again; xcb skips accounts at a known limit."],
        [<code key="c">needs_input</code>, <>The provider stopped with a question, in <code>text</code>.</>, <>Answer in a new request, or reopen <code>session</code>.</>],
        [<code key="c">custody_unproven</code>, "xcb couldn’t confirm the provider stopped, so it keeps the account held.", <>Don’t retry on that account; run <code>xcb recover</code>.</>],
      ]} />
      <p>When a request provably started no provider, the failure also carries <code>joined: true</code> and <code>effects: &quot;none&quot;</code>. Once a session exists, those facts come from <code>outcome</code> instead. SIGINT and SIGTERM cancel the turn the same way <code>timeoutMs</code> does; killing xcb doesn’t prove the provider stopped.</p>
      <h2 id="agents">Notes for agents</h2>
      <ul>
        <li>One call is one turn. Multi-step plans and retries are your loop; accounts that failed on a usage limit are skipped on the next call.</li>
        <li>A request can’t carry tools, hooks, system prompts, credentials, or provider flags.</li>
        <li>Read accounts, models, and provider status with <code>xcb --json accounts</code>, <code>xcb --json models</code>, and <code>xcb --json doctor</code>.</li>
      </ul>
      <p>The <Ext href={`${repositoryDocs}/route.md`}>route contract</Ext> in the repository is the full reference.</p>
      <h2 id="sdk">Embedding in an app</h2>
      <p>To run tasks from inside a TypeScript application on accounts your app manages, use the <a href="/docs/sdk">TypeScript SDK</a>. Unlike <code>xcb --json route</code>, the SDK doesn’t choose the account or model: your app names both, and the SDK holds that account while the task runs.</p>
    </>
  );
}

function Sdk() {
  return (
    <>
      <p>The TypeScript SDK’s <code>createSubscriptionRouter</code> runs one task at a time on the account and model your app names, and holds that account until the provider process has exited. Your app names both: to let xcb choose them, call <a href="/docs/route"><code>xcb --json route</code></a> instead.</p>
      <h2 id="install">Install</h2>
      <p>The SDK is the <code>@hraness/xcb</code> package on npm. It runs on Node 22.13 or later and Bun 1.3.14 or later.</p>
      <Code>{`npm install @hraness/xcb
# or
bun add @hraness/xcb`}</Code>
      <p>The package also installs the <code>xcb-compat</code> command, which is separate from the native <code>xcb</code>.</p>
      <h2 id="example">A complete example</h2>
      <p>This program runs as is. It uses a stand-in adapter that starts no provider and echoes the prompt, so you can watch the router hold the account during the task and release it after. Save it as <code>router-demo.ts</code> in the project where you installed the SDK:</p>
      <Code>{sdkExample}</Code>
      <p>Run it with <code>node router-demo.ts</code> (Node 24 or later runs TypeScript directly) or <code>bun router-demo.ts</code>. It prints:</p>
      <Code>{sdkExampleOutput}</Code>
      <h2 id="parts">What each part does</h2>
      <ul>
        <li><strong>Account store:</strong> <code>SqliteAccountLeases</code> records which task holds each account. While one task holds an account, another <code>run</code> on it fails with <code>ACCOUNT_BUSY_OR_RECOVERY_REQUIRED</code>.</li>
        <li><strong>Capability profile and broker:</strong> the tools a model may call, bound to one workspace and run. <code>isActive</code> lets your app revoke them mid-task.</li>
        <li><strong>Adapter:</strong> starts the provider, runs the turn, and proves in <code>stop</code> that the provider’s processes have exited. The router releases the account only after <code>stop</code> returns that proof.</li>
        <li><strong>Request:</strong> the account, model, prompt, and limits. <code>maxRunMs</code> plus <code>maxCleanupMs</code> is the whole deadline, at most one hour; pass <code>signal</code> to cancel sooner.</li>
      </ul>
      <h2 id="adapters">Use a real provider</h2>
      <p>Replace the stand-in with an adapter that launches a provider: <code>createClaudeTaskAdapter</code> (Claude Code), <code>createClaudeApiAdapter</code> (the Claude API), <code>createCodexTaskAdapter</code> or <code>createCodexManagedTaskAdapter</code> (Codex). The Devin adapter is present but not enabled for tasks. A real adapter runs only with a qualification record from your host: evidence that its exact provider build runs with the expected tools, configuration, and file access. The SDK checks that record on every task; it doesn’t create it for you, and it doesn’t discover credentials. The <Ext href={`${repositoryDocs}/compatibility.md#embedding-the-subscription-router`}>compatibility reference</Ext> documents each adapter’s options.</p>
      <h2 id="errors">Errors</h2>
      <p>The router throws an <code>Error</code> whose message is a code:</p>
      <ul>
        <li><code>ROUTER_ROUTE_REQUIRED</code>, <code>ROUTER_ROUTE_UNAVAILABLE</code>, or <code>ROUTER_ROUTE_AMBIGUOUS</code>: name a registered provider, or pass the exact <code>route</code> when several adapters serve one provider.</li>
        <li><code>ACCOUNT_BUSY_OR_RECOVERY_REQUIRED</code>: another task holds the account, or a stopped task’s hold wasn’t released.</li>
        <li><code>TASK_ADAPTER_UNQUALIFIED</code> or <code>TASK_QUALIFICATION_MISMATCH</code>: the adapter’s qualification record is missing, expired, or for another build.</li>
        <li><code>TASK_CUSTODY_UNPROVEN</code>: the adapter couldn’t prove the provider stopped, so the account stays held. Recover it only with independent proof that the process exited.</li>
      </ul>
    </>
  );
}

function ApplicationApi() {
  return (
    <>
      <p>The application API gives your app one model response per call. xcb handles provider sign-in and the provider process; your app keeps its own data and actions. Each call takes a prompt and returns text, with no tools, hooks, saved history, continuation, or switching to another account.</p>
      <h2 id="discover">Find available accounts and models</h2>
      <Code>{`xcb --json generate --capabilities`}</Code>
      <p>This reads local records without provider refresh or inference. Use only an account with <code>available: true</code> and one of its exact model keys. <code>supported: true</code> alone doesn’t mean an account is available.</p>
      <p>The command checks the local executables and can take several seconds. Give it its own timeout, separate from the generation deadline; 90 seconds is a practical desktop integration recommendation, not a protocol timing guarantee.</p>
      <p>When your app launches xcb, preserve <code>HOME</code> or set <code>XCB_STATE</code> in the child environment, even when passing <code>--state</code>. Close unused stdin and drain stdout and stderr while waiting for the process to finish.</p>
      <Note>Application checks are separate from coding support. A fresh install reports <code>supported: false</code> until that xcb build, provider, account, and model pass xcb’s application checks. Signing in or a passing <code>doctor</code> isn’t enough, and <code>xcb run</code> is not a substitute when generation is unavailable.</Note>
      <h2 id="generate">Generate one response</h2>
      <p>Start xcb with <code>--json generate</code>, write one UTF-8 JSON document to stdin, close stdin, and drain stdout and stderr while waiting for the result. Use the account and model values from capabilities:</p>
      <Code>{`{
  "version": 1,
  "account": "<available-account-id>",
  "model": "<full-model-key>",
  "prompt": "Summarize the supplied text in one sentence.",
  "timeoutMs": 60000,
  "maxOutputBytes": 65536
}`}</Code>
      <p>All six fields are required and other fields are rejected. The whole input is limited to 1 MiB, <code>timeoutMs</code> to 1,000 to 300,000 milliseconds, and <code>maxOutputBytes</code> to 1 to 262,144 bytes. The prompt can’t be empty or contain NUL.</p>
      <p>A success has <code>status: completed</code>, the generated <code>text</code>, and an outcome showing the provider exited with no effects. A failure exits nonzero with a fixed error object and no text. Treat the text as untrusted: validate it against your app’s own schema before acting on it.</p>
      <h2 id="host-responsibilities">Keep your app’s actions in your app</h2>
      <p>xcb does the inference, not your app’s file access, network requests, or message delivery. Your app chooses recipients, authorizes actions, validates output, and keeps its own request records. <Ext href="https://github.com/hraness/textbutler">Textbutler</Ext> is an example app that keeps contact access and message approval in its own code.</p>
      <p>Send SIGINT or SIGTERM to cancel, then wait for xcb to exit. The deadline passing doesn’t prove the provider stopped. When the result is uncertain, xcb keeps the account held; don’t retry blindly.</p>
      <h2 id="checks">Application checks and renewal</h2>
      <p>An app route becomes available after a host check of the sandbox and a separate fixed live test. The result expires no later than 24 hours after the check starts, and sooner if xcb, the provider, or the account’s credentials change. Reading capabilities never extends their lifetime.</p>
      <p>Follow the <Ext href={`${repository}/blob/main/qualification/README.md#application-qualification-prerequisites`}>host check procedure</Ext> before sending app traffic. On macOS, the <Ext href={`${repositoryDocs}/application-renewal.md`}>Claude renewal helper</Ext> renews one checked deployment and account; installing xcb doesn’t turn it on. The <Ext href={`${repositoryDocs}/application-api.md`}>application API reference</Ext> has the full schemas and error codes.</p>
    </>
  );
}

const commandGroups: readonly Readonly<{ id: string; title: string; commands: readonly (readonly [string, string])[] }>[] = [
  { id: "commands-start", title: "Start here", commands: [
    ["setup <provider>", "Add an account, check the provider, sign in, and load its models"],
    ["chat", "Open your thread; --new starts a project view, --resume <id> reopens one"],
    ["run -p <task>", "Run one task in --cwd and print the answer; --account, --model, --image"],
    ["doctor", "Check provider builds and unfinished runs; --provider, --executable"],
  ] },
  { id: "commands-accounts", title: "Accounts and models", commands: [
    ["accounts", "List accounts; add, login, token, refresh, default, disable, enable, import-codex, import-devin, import-agentmixer"],
    ["models", "List models; refresh, default, tiers, route"],
    ["offers", "Show public plan offers (not checked against your account)"],
  ] },
  { id: "commands-tasks", title: "Conversations and tasks", commands: [
    ["conversations", "List your thread and project views"],
    ["workspaces", "List, add, hide, and show project folders; why explains a task's folder"],
    ["history <id>", "Read a conversation's saved messages"],
    ["rename <id> <title>", "Rename a conversation"],
    ["tasks", "List tasks; show, messages, verify, cancel"],
    ["backlog", "Manage a project's work queue"],
    ["steer <task> <text>", "Queue guidance for a task's next turn"],
    ["watch <target> <source>", "Send a task's completion report to another task"],
    ["inbox", "See guidance and reports and whether they arrived"],
    ["attention", "Show questions and approvals waiting on you"],
    ["schedules", "Manage recurring prompts"],
    ["sessions", "List direct provider sessions; rm, prune, export"],
    ["resume [id]", "Reopen a direct provider session"],
  ] },
  { id: "commands-setup", title: "Setup", commands: [
    ["service", "Start the background supervisor at login (macOS); install, status, uninstall, plan"],
    ["update", "Check for updates and set the update policy; check, status, enable, disable"],
    ["upgrade [version]", "Install the latest, or a named, verified release"],
    ["completions <shell>", "Print shell completions"],
  ] },
  { id: "commands-advanced", title: "Advanced (xcb help advanced)", commands: [
    ["link, fleet, dispatch, send, remote", "Link machines through a relay and run work on them"],
    ["daemons, projects, memory, reflex", "Project agents, project grants, Wordcell notes, learned routing"],
    ["panes, plugins, hooks, judge", "Terminal panes, extensions, lifecycle hooks, the routing judge"],
    ["config, recover, command, generate", "Settings, unfinished runs, command-runner jobs, app text generation"],
  ] },
  { id: "commands-programs", title: "For programs", commands: [
    ["--json route", "Route one task from stdin and print one JSON result"],
    ["--json generate", "One tool-free model response for an app"],
  ] },
];

const configKeys: readonly (readonly [string, string, string])[] = [
  ["turn_timeout_ms", "1800000", "Deadline for one provider turn, 1,000 to 3,600,000 ms"],
  ["default_account", "null", "Account for new direct sessions; set with xcb accounts default"],
  ["favorites", "built-in list", "Preferred models that break ties when routing, up to 128"],
  ["auto_failover", "true", "Continue a managed task on another route after a usage limit"],
  ["pane", "\"focus\"", "Pane shown in direct sessions"],
  ["reduced_motion", "false", "Turn off terminal animation"],
  ["extensions.auto_continue", "enabled, 3 in a row, 600000 ms", "max_consecutive 1 to 16; max_elapsed_ms 1,000 to 3,600,000"],
  ["extensions.gobstopper", "enabled at 250,000 tokens", "Context management: trigger_tokens, floor_tokens, min_interval_ms, min_savings_tokens"],
  ["extensions.usage", "true", "Local usage measurement"],
  ["extensions.hooks", "false", "Allow hooks to run"],
  ["extensions.aicharts_export", "false", "Allow local usage exports (xcb sessions export)"],
  ["extensions.judge", "enabled: false", "enabled, model, endpoint for the optional judge"],
  ["extensions.reflexes", "route active, settle auto, confirm auto, learn true", "off, observe, active, or auto (settle and confirm only)"],
];

function Reference() {
  return (
    <>
      <p>This page lists what xcb reads and writes. Run <code>xcb &lt;command&gt; --help</code> for any command’s flags.</p>
      <h2 id="options">Global options</h2>
      <Table label="Global options" head={["Option", "Effect"]} rows={[
        [<code key="o">--state &lt;dir&gt;</code>, <>State folder; the default is <code>$XCB_STATE</code> or <code>~/.local/share/xcb</code></>],
        [<code key="o">--json</code>, "Machine-readable output where a command supports it"],
        [<code key="o">--cwd &lt;dir&gt;</code>, <>Project hint for the thread; the exact folder for <code>run</code>, <code>chat --new</code>, and <code>models route</code> (default <code>.</code>)</>],
        [<code key="o">-h, --help</code>, <>Help; <code>xcb help advanced</code> lists the rest of the commands</>],
        [<code key="o">-V, --version</code>, "The installed version"],
      ]} />
      <h2 id="commands">Commands</h2>
      <p>Grouped as in <code>xcb --help</code>. Plain <code>xcb</code> opens your thread in a terminal and prints a short start screen anywhere else.</p>
      {commandGroups.map((group) => (
        <section aria-labelledby={group.id} key={group.id}>
          <h3 id={group.id}>{group.title}</h3>
          <Table label={`${group.title} commands`} head={["Command", "What it does"]} rows={group.commands.map(([command, summary]) => [<code key="c">{`xcb ${command}`}</code>, summary])} />
        </section>
      ))}
      <h2 id="configuration">Configuration</h2>
      <p><code>config.json</code> in the state folder holds your settings; <code>xcb config</code> prints the settings in effect. Keys you leave out use their defaults. An unknown key or an out-of-range value makes xcb refuse the file.</p>
      <Table label="config.json keys" head={["Key", "Default", "Values"]} rows={configKeys.map(([key, value, note]) => [<code key="k">{key}</code>, value, note])} />
      <p>Change extensions with <code>xcb plugins enable|disable &lt;name&gt;</code> and the judge with <code>xcb judge</code>. The update policy (<code>notify</code>, <code>auto</code>, or <code>disable</code>) is kept separately and set with <code>xcb update</code>.</p>
      <h2 id="environment">Environment variables</h2>
      <Table label="Environment variables" head={["Variable", "Effect"]} rows={[
        [<code key="e">XCB_STATE</code>, <>State folder, instead of <code>~/.local/share/xcb</code></>],
        [<code key="e">XCB_COORDINATION_ROOT</code>, <>Folder for the locks that let one writer change a project at a time, instead of <code>~/.local/share/xcb-coordination</code>. Every cooperating xcb process must use the same value.</>],
        [<code key="e">XCB_JEV_API_KEY</code>, <>Judge key, instead of the stored one. <code>TYPESAFE_API_KEY</code> also works.</>],
        [<code key="e">XCB_JEV_URL</code>, "Custom judge endpoint; requires a key from the environment"],
        [<code key="e">XCB_JEV_MODEL</code>, "Judge model"],
        [<code key="e">XCB_RELAY_URL</code>, <>Default relay for <code>xcb link</code></>],
        [<code key="e">NO_COLOR</code>, "Any non-empty value turns off color"],
        [<code key="e">VISUAL</code>, <>Editor opened by Ctrl-G, then <code>EDITOR</code></>],
        [<code key="e">XCB_VERSION</code>, <>Installer only: install this release instead of building from source</>],
        [<code key="e">XCB_INSTALL_PREFIX</code>, <>Installer only: install under this prefix instead of <code>~/.local</code></>],
        [<code key="e">XCB_ADD_PATH</code>, <>Installer only: <code>yes</code> adds the <code>bin</code> folder to your shell profile</>],
      ]} />
      <h2 id="files">Files and folders</h2>
      <ul>
        <li><code>~/.local/share/xcb</code>: accounts, credentials, conversations, tasks, <code>config.json</code>, the update policy, and, for installer installs, <code>install.json</code> and a copy of the installer. <code>qualification/linux.json</code> holds the Linux sandbox check, and <code>reflexes/</code> any replacement reflex programs.</li>
        <li><code>~/.local/share/xcb-command</code>: the command runner’s VM, caches, and job records.</li>
        <li><code>~/.local/share/xcb-coordination</code>: write locks shared by every xcb process.</li>
        <li><code>~/Library/LaunchAgents/dev.hraness.xcb.update.plist</code> and <code>dev.hraness.xcb.habitat.*.plist</code> (macOS): the daily update check and the login service, when you turn them on.</li>
      </ul>
      <p>The TypeScript <code>xcb-compat</code> CLI uses <code>~/.xcb</code>. Keep the two state folders separate.</p>
      <h2 id="exit-codes">Exit codes</h2>
      <Table label="Exit codes" head={["Code", "Meaning"]} rows={[
        ["0", "Success"],
        ["1", <>Failure. <code>xcb run</code> and <code>xcb --json route</code> also exit 1 unless the turn completed, the provider exited, and nothing waits for an answer. <code>xcb doctor</code> exits 1 when it finds no provider.</>],
        ["2", <>Usage error: an unknown command or flag. <code>xcb remote status --wait</code> also exits 2 for an unknown command ID or an exhausted wait.</>],
      ]} />
      <h2 id="json">JSON output</h2>
      <p>With <code>--json</code>, or when xcb detects that an agent is running it, a failed command prints one line on stdout:</p>
      <Code>{`{"ok":false,"error":{"code":"unavailable","message":"No account matches \\"zz\\".","next":"xcb accounts"}}`}</Code>
      <p><code>code</code> is one of <code>usage</code>, <code>invalid-input</code>, <code>unavailable</code>, <code>conflict</code>, <code>local-io</code>, <code>local-database</code>, <code>invalid-record</code>, <code>private-state</code>, <code>provider-not-started</code>, <code>provider-protocol</code>, or <code>cleanup-unproven</code>; <code>next</code> is the command to run next, or <code>null</code>. Successful JSON output carries <code>&quot;version&quot;: 1</code>. <code>xcb --json run</code> prints the <code>session</code>, <code>state</code>, <code>outcome</code>, and <code>text</code>; see <a href="/docs/route#response">route results</a> for the same fields.</p>
    </>
  );
}

export function TopicContent({ slug }: { slug: DocsSlug }) {
  switch (slug) {
    case "getting-started": return <GettingStarted />;
    case "how-routing-works": return <HowRoutingWorks />;
    case "security": return <Security />;
    case "projects-and-tasks": return <ProjectsAndTasks />;
    case "providers": return <Providers />;
    case "workspace": return <Workspace />;
    case "customization": return <Customization />;
    case "reflexes": return <Reflexes />;
    case "upgrade-and-uninstall": return <UpgradeAndUninstall />;
    case "troubleshooting": return <Troubleshooting />;
    case "route": return <RouteTasks />;
    case "sdk": return <Sdk />;
    case "application-api": return <ApplicationApi />;
    case "reference": return <Reference />;
  }
}

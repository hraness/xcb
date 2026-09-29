import type { Metadata } from "next";
import { AskAiAboutThis } from "@hraness/ui";
import { CodeBlock } from "../code-block";
import { supportedBuilds } from "../docs/provider-status";
import { PageHeader, PageSection } from "../page-header";
import { publishedRelease } from "../publication";
import { releaseStatusLabel } from "../release-state";
import { SiteHeader } from "../site-header";
import { socialImages } from "../social";
import { agentPrompt, installCommand, pathCommand } from "./commands";

const title = "Install xcb";
const description = "Install xcb with one command on macOS with Apple silicon or Linux x86_64, connect a Claude, Codex, or Devin account, and give it a first task.";

export const metadata: Metadata = {
  title,
  description,
  alternates: { canonical: "/install" },
  openGraph: { title, description, siteName: "Excalibur (xcb)", type: "website", url: "/install", images: socialImages },
  twitter: { card: "summary_large_image", title, description, images: socialImages },
};

const sourceBuild = `git clone https://github.com/hraness/xcb.git
cd xcb
rustup toolchain install 1.97.1 --profile minimal
./scripts/install-native.sh`;

const firstTask = `cd ~/path/to/your/project
xcb`;

const routeExample = `echo '{"version":1,"workspace":"'"$PWD"'","task":"Fix the failing parser test","dryRun":true}' \\
  | xcb --json route`;

const uninstall = `xcb service uninstall   # macOS: stop starting the supervisor at login
xcb update disable      # stop the daily update check
rm ~/.local/bin/xcb`;

export default function Install() {
  const released = publishedRelease !== null;
  return (
    <div data-hraness-marketing-preset="minimal" className="xcb-install-page">
      <SiteHeader active="install" />
      <main id="main" tabIndex={-1} className="xcb-page">
        <PageHeader
          id="install-page-title"
          title={title}
          lead={released
            ? "One command installs xcb. Then connect an account and give it a task. It takes a few minutes, and nothing runs as root."
            : "No release is published yet. Build xcb from source with the steps at the end of this page."}
          meta={releaseStatusLabel(publishedRelease)}
        />

        <PageSection id="before" title="Before you start">
          <ul>
            <li>A Mac with Apple silicon, or a Linux x86_64 machine with glibc 2.34 or newer (Ubuntu 22.04, Debian 12, RHEL 9, and later). Other systems can <a href="#source">build from source</a>.</li>
            <li>On Linux, complete the <a href="/docs/providers#claude-on-linux">sandbox checks for Claude</a> after installing xcb and before connecting an account. Codex and Devin run on macOS only.</li>
            <li>For Claude: a Claude subscription and Claude Code {supportedBuilds.claudeMinimum} or later. Check with <code>claude --version</code>.</li>
            <li>For Codex (macOS): Codex CLI {supportedBuilds.codex.join(" or ")}, which <code>npm install -g @openai/codex@{supportedBuilds.codex[0]}</code> installs.</li>
            <li>For Devin (macOS): Devin CLI {supportedBuilds.devin[0]}, signed in with <code>devin auth login</code>.</li>
          </ul>
        </PageSection>

        {released
          ? (
            <PageSection id="install" title="1. Install xcb">
              <p>Paste this into your terminal. It downloads the release for your computer, checks its SHA-256 checksum, and puts <code>xcb</code> in <code>~/.local/bin</code>:</p>
              <CodeBlock code={installCommand} />
              <p>When it finishes, <code>xcb --version</code> prints the version. If your shell says it can’t find <code>xcb</code>, add this line to your shell profile (<code>~/.zshrc</code> on a Mac) and open a new terminal:</p>
              <CodeBlock code={pathCommand} />
            </PageSection>
          )
          : null}

        <PageSection id="connect" title={released ? "2. Connect an account" : "Connect an account"}>
          <CodeBlock code="xcb setup claude" />
          <p>This adds a Claude account, checks your Claude Code build, opens the sign-in page in your browser, and loads the account’s models. xcb keeps this sign-in in its own folder, so your usual Claude Code login is unaffected. Codex and Devin have their own steps in <a href="/docs/providers">Accounts and models</a>.</p>
        </PageSection>

        <PageSection id="first-task" title={released ? "3. Give it a task" : "Give it a task"}>
          <CodeBlock code={firstTask} />
          <p>This opens your thread. Type a task, such as <em>fix the failing test in add.js</em>, and press Enter. xcb says which project folder it chose, picks an account and model that can take the task, and shows the answer when the task finishes. Close the terminal whenever you like: the task keeps running, and the next <code>xcb</code> shows the result. <code>/tasks</code> lists your work and <code>/help</code> lists every command.</p>
          <p>The <a href="/docs/getting-started">getting started guide</a> walks through a first task step by step.</p>
        </PageSection>

        <PageSection id="agent" title="Ask your agent to install it">
          <p>Already using Claude Code, Codex, or another coding agent? Paste this prompt into it. It installs and checks xcb, then leaves the sign-in to you:</p>
          <CodeBlock code={agentPrompt} language="text" copyLabel="Copy prompt" />
        </PageSection>

        <PageSection id="build-on" title="Build on xcb">
          <p>From another agent or a script, <code>xcb --json route</code> takes one task as JSON, picks an account and model, and returns one JSON result. With <code>dryRun</code> it shows the route without running anything:</p>
          <CodeBlock code={routeExample} />
          <p>In a TypeScript app, install the SDK. There your app names the account and model for each task:</p>
          <CodeBlock code="npm install @hraness/xcb" />
          <p>The <a href="/docs/route">route guide</a> and the <a href="/docs/sdk">SDK quickstart</a> have the details.</p>
        </PageSection>

        <PageSection id="update" title="Update and uninstall">
          <p><code>xcb upgrade</code> installs the newest release the same way the installer did. To remove xcb, let running tasks finish, then run:</p>
          <CodeBlock code={uninstall} />
          <p>Your accounts, credentials, and history stay in <code>~/.local/share/xcb</code> until you delete that folder. <a href="/docs/upgrade-and-uninstall">Upgrade and uninstall</a> lists everything xcb creates.</p>
        </PageSection>

        <PageSection id="source" title="Build from source">
          <p>On an Intel Mac, ARM Linux, or any other system, build with Git, Rust 1.97.1, and your platform’s build tools:</p>
          <CodeBlock code={sourceBuild} />
        </PageSection>
      </main>
      <AskAiAboutThis className="ask-ai" url="https://xcb.sh/install" />
    </div>
  );
}

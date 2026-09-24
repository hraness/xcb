"use client";

import { useState } from "react";

const views = ["Session", "Accounts", "Usage"] as const;
type View = typeof views[number];

export function WorkspacePreview() {
  const [view, setView] = useState<View>("Session");
  return (
    <figure className="xcb-preview">
      <div className="xcb-terminal hraness-material-pane">
        <div className="xcb-terminal-bar"><span><span aria-hidden="true">†</span> xcb <span className="xcb-terminal-path">/ your-project</span></span><span className="xcb-terminal-local">local workspace</span></div>
        <div className="xcb-preview-controls" role="group" aria-label="Illustrative workspace views">
          {views.map((label) => <button key={label} type="button" aria-pressed={view === label} aria-controls="workspace-example" onClick={() => setView(label)}>{label}</button>)}
          <span className="xcb-preview-hint">Explore the workspace</span>
        </div>
        <div className="xcb-terminal-body" id="workspace-example" aria-live="polite" aria-atomic="true">
          {view === "Session" && <>
            <p className="xcb-terminal-prompt">Run the tests. Fix the failure. Show me the diff.</p>
            <ol className="xcb-terminal-steps">
              <li><span className="xcb-step-mark">01</span><span><strong>Inspect the project</strong><small>Read files and find the relevant test.</small></span><span className="xcb-step-result">read</span></li>
              <li><span className="xcb-step-mark">02</span><span><strong>Test, repair, test again</strong><small>Run prepared dependencies in the isolated Linux runner.</small></span><span className="xcb-step-result">run</span></li>
              <li><span className="xcb-step-mark">03</span><span><strong>Review what changed</strong><small>Keep the diff and session history close to the work.</small></span><span className="xcb-step-result">review</span></li>
            </ol>
            <div className="xcb-terminal-input"><span aria-hidden="true">›</span> /sessions <span>pick up where you left off</span></div>
          </>}
          {view === "Accounts" && <>
            <p className="xcb-terminal-prompt">Pick the account and model yourself.</p>
            <div className="xcb-example-accounts">
              <div><strong>Claude</strong><span>Named accounts</span><code>/accounts</code></div>
              <div><strong>Codex</strong><span>Observed model catalog</span><code>/model</code></div>
              <div><strong>Devin</strong><span>Native ACP adapter</span><span>source preview</span></div>
            </div>
            <p className="xcb-preview-note">Connect your own accounts. Provider access and supported builds are checked on your machine.</p>
            <div className="xcb-terminal-input"><span aria-hidden="true">›</span> /accounts <span>choose before you run</span></div>
          </>}
          {view === "Usage" && <>
            <p className="xcb-terminal-prompt">Know what you can use next.</p>
            <dl className="xcb-example-usage">
              <div><dt>Session activity</dt><dd>Local token observations</dd></div>
              <div><dt>Known Claude quota limit</dt><dd>Wait until the reported reset</dd></div>
              <div><dt>Missing or stale usage</dt><dd>Shown as unknown</dd></div>
            </dl>
            <p className="xcb-preview-note">Usage measurement stays on your machine.</p>
            <div className="xcb-terminal-input"><span aria-hidden="true">›</span> /accounts <span>see known quota windows</span></div>
          </>}
        </div>
      </div>
      <figcaption>Illustrative workspace · no live provider calls. <a href="/docs/providers">Current provider support →</a></figcaption>
    </figure>
  );
}

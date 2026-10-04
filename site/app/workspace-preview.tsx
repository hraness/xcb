"use client";

import { useState } from "react";

const views = ["Session", "Accounts", "Usage"] as const;
type View = typeof views[number];

export function WorkspacePreview() {
  const [view, setView] = useState<View>("Session");
  const index = views.indexOf(view);
  const select = (next: number) => setView(views[(next + views.length) % views.length]);
  return (
    <figure className="xcb-preview">
      <div className="xcb-terminal hraness-material-pane">
        <div className="xcb-terminal-bar"><span><span aria-hidden="true">†</span> xcb <span className="xcb-terminal-path">/ your-project</span></span><span className="xcb-terminal-local">local workspace</span></div>
        <div className="xcb-terminal-body" id="workspace-example" aria-live="polite" aria-atomic="true">
          {view === "Session" && <>
            <p className="xcb-terminal-prompt">A clear path from a request to a finished change.</p>
            <div className="xcb-visual-flow">
              <div><span className="xcb-visual-icon" aria-hidden="true">⌕</span><strong>Understand</strong><small>Find the right files and test.</small></div>
              <span className="xcb-visual-arrow" aria-hidden="true">→</span>
              <div><span className="xcb-visual-icon" aria-hidden="true">✦</span><strong>Work</strong><small>Run, repair, and check again.</small></div>
              <span className="xcb-visual-arrow" aria-hidden="true">→</span>
              <div><span className="xcb-visual-icon" aria-hidden="true">✓</span><strong>Share</strong><small>Review the change with context.</small></div>
            </div>
            <div className="xcb-terminal-input"><span aria-hidden="true">›</span> Sessions <span>pick up where you left off</span></div>
          </>}
          {view === "Accounts" && <>
            <p className="xcb-terminal-prompt">Your accounts. An explicit choice.</p>
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
            <p className="xcb-preview-note">No invented balances. Local measurement stays separate from optional publishing.</p>
            <div className="xcb-terminal-input"><span aria-hidden="true">›</span> /accounts <span>see known quota windows</span></div>
          </>}
        </div>
      </div>
      <div className="xcb-preview-controls" role="group" aria-label="Illustrative workspace views">
        {views.map((label, viewIndex) => <button key={label} type="button" aria-pressed={view === label} aria-controls="workspace-example" onClick={() => setView(label)}><span className="xcb-tab-number">{String(viewIndex + 1).padStart(2, "0")}</span><span>{label}</span></button>)}
        <span className="xcb-preview-hint">Explore the workspace</span>
      </div>
      <div className="xcb-preview-nav" aria-label="Preview navigation"><button type="button" aria-label="Previous workspace view" onClick={() => select(index - 1)}>‹</button><span>{String(index + 1).padStart(2, "0")} / {String(views.length).padStart(2, "0")}</span><button type="button" aria-label="Next workspace view" onClick={() => select(index + 1)}>›</button></div>
      <figcaption>Illustrative workspace · no live provider calls. <a href="/docs/providers">Current provider support ↗</a></figcaption>
    </figure>
  );
}

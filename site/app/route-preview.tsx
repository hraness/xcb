"use client";

import { useState } from "react";

const views = ["Agent", "SDK"] as const;
type View = typeof views[number];

export function RoutePreview() {
  const [view, setView] = useState<View>("Agent");
  return (
    <figure className="xcb-preview">
      <div className="xcb-terminal hraness-material-pane">
        <div className="xcb-terminal-bar"><span><span aria-hidden="true">†</span> xcb <span className="xcb-terminal-path">route</span></span><span className="xcb-terminal-local">subscription router</span></div>
        <div className="xcb-preview-controls" role="group" aria-label="Illustrative router contracts">
          {views.map((label) => <button key={label} type="button" aria-pressed={view === label} aria-controls="route-example" onClick={() => setView(label)}>{label}</button>)}
          <span className="xcb-preview-hint">Two ways in</span>
        </div>
        <div className="xcb-terminal-body xcb-terminal-code" id="route-example" aria-live="polite" aria-atomic="true">
          {view === "Agent" && <pre className="xcb-code" tabIndex={0}><code>{`$ xcb --json route
→ { "version": 1,
    "workspace": "/your-project",
    "task": "Fix the failing parser test" }

← { "status": "completed",
    "route": { "provider": "claude",
               "account": "a_9f2c…",
               "model": "claude/sonnet/low" },
    "session": "s_…",
    "outcome": { "terminal": "completed",
                 "joined": true } }`}</code></pre>}
          {view === "SDK" && <pre className="xcb-code" tabIndex={0}><code>{`const router = createSubscriptionRouter({ leases, adapters });

const result = await router.run({
  provider: "claude",
  accountId: "a_9f2c…",
  profile, model,
  purpose: "respond",
  prompt: "Summarize the diff in this workspace.",
  limits: { maxRunMs: 60_000, maxCleanupMs: 10_000,
            maxOutputBytes: 65_536 },
}, broker);

// result.outcome.status === "completed"
// the account is released only after the provider stops`}</code></pre>}
        </div>
      </div>
      <figcaption>Illustrative requests · the SDK is built from source. <a href="/docs/route">Route tasks →</a></figcaption>
    </figure>
  );
}

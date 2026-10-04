"use client";

import { useState } from "react";

const views = ["Agent", "SDK"] as const;
type View = typeof views[number];

export function RoutePreview() {
  const [view, setView] = useState<View>("Agent");
  const index = views.indexOf(view);
  const select = (next: number) => setView(views[(next + views.length) % views.length]);
  return (
    <figure className="xcb-preview">
      <div className="xcb-terminal hraness-material-pane">
        <div className="xcb-terminal-bar"><span><span aria-hidden="true">†</span> xcb <span className="xcb-terminal-path">route</span></span><span className="xcb-terminal-local">subscription router</span></div>
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
// custody released only after stop is proven`}</code></pre>}
        </div>
      </div>
      <div className="xcb-preview-controls" role="group" aria-label="Illustrative router contracts">
        {views.map((label, viewIndex) => <button key={label} type="button" aria-pressed={view === label} aria-controls="route-example" onClick={() => setView(label)}><span className="xcb-tab-number">{String(viewIndex + 1).padStart(2, "0")}</span><span>{label}</span></button>)}
        <span className="xcb-preview-hint">Two ways in</span>
      </div>
      <div className="xcb-preview-nav" aria-label="Preview navigation"><button type="button" aria-label="Previous router view" onClick={() => select(index - 1)}>‹</button><span>{String(index + 1).padStart(2, "0")} / {String(views.length).padStart(2, "0")}</span><button type="button" aria-label="Next router view" onClick={() => select(index + 1)}>›</button></div>
      <figcaption>Closed contracts · source-built SDK with host-supplied qualification. <a href="/docs/route">Route contract ↗</a></figcaption>
    </figure>
  );
}

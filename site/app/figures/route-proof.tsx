import { MarketingProofFrame, SyntaxCode } from "@hraness/design-kit/react/server";

/**
 * The hero's proof object: a dry run of `xcb --json route`, the one command
 * another agent or app uses to hand xcb a task. The request and response are
 * the documented shapes from docs/route.md on the current release; ids are
 * shortened the way the reference shortens them.
 */
export const routeProofRequest = `{
  "version": 1,
  "workspace": "/absolute/path/to/project",
  "task": "Fix the failing parser test and show the diff",
  "dryRun": true
}`;

export const routeProofResponse = `{
  "version": 1,
  "status": "selected",
  "requestId": "route_…",
  "route": {
    "provider": "claude",
    "account": "a_…",
    "model": "claude/sonnet/low",
    "label": "Sonnet · low",
    "reason": "deterministic fallback · classifier not available · standard tier · balanced task · Pareto P1 · quality 92 · relative cost 55 · relative latency 50"
  }
}`;

export function RouteProof() {
  return (
    <MarketingProofFrame
      chrome="terminal"
      className="xcb-route-proof"
      title="xcb --json route < task.json"
      credit="Dry run as documented in docs/route.md for v0.20.11, checked 2026-10-13. With dryRun false the same call runs the task and returns its text."
    >
      <pre className="xcb-route-proof__pre" tabIndex={0}>
        <span className="xcb-route-proof__comment">{"# task.json\n"}</span>
        <SyntaxCode code={routeProofRequest} language="json" styles="classes" />
        {"\n\n"}
        <span className="xcb-route-proof__comment">{"# xcb --json route < task.json\n"}</span>
        <SyntaxCode code={routeProofResponse} language="json" styles="classes" />
      </pre>
    </MarketingProofFrame>
  );
}

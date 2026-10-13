import { MarketingProofFrame, SyntaxCode } from "@hraness/design-kit/react/server";

/**
 * The integration example: a dry run of `xcb --json route`, the one command
 * another agent or app uses to hand xcb a task. The response is the documented
 * shape from docs/route.md on the current release; ids are shortened the way
 * the reference shortens them.
 */
export const routeProofResponse = `{
  "version": 1,
  "status": "selected",
  "requestId": "route_…",
  "route": {
    "provider": "claude",
    "account": "a_…",
    "model": "claude/sonnet/low",
    "label": "Sonnet · low",
    "reason": "deterministic fallback · balanced task · Pareto P1 · quality 92"
  }
}`;

export function RouteProof() {
  return (
    <MarketingProofFrame
      chrome="terminal"
      className="xcb-route-proof"
      title="xcb --json route < task.json"
      credit="Dry run as documented in docs/route.md for v0.20.11, checked 2026-10-13."
    >
      <pre className="xcb-route-proof__pre" tabIndex={0}>
        <span className="xcb-route-proof__comment">{"# task.json sets \"dryRun\": true\n"}</span>
        <SyntaxCode code={routeProofResponse} language="json" styles="classes" />
      </pre>
    </MarketingProofFrame>
  );
}

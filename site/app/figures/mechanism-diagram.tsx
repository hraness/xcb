import { DiagramArrowhead, MarketingDiagram } from "@hraness/design-kit/react/server";

/**
 * How work moves: an agent (or you) uses xcb's primitives, xcb runs each task
 * on one of your accounts, and the backlog and attention queue carry state
 * back. Every box names a real command group from the reference.
 */
const primitives = ["projects", "tasks", "schedules", "routes", "hooks"] as const;

const width = 720;
const height = 300;

export function MechanismDiagram() {
  const agentBox = { x: 16, y: 96, w: 150, h: 108 };
  const hubBox = { x: 246, y: 24, w: 228, h: 252 };
  const accountBox = { x: 554, y: 96, w: 150, h: 108 };
  return (
    <MarketingDiagram
      className="xcb-mechanism"
      width={width}
      height={height}
      label="Your agent uses xcb's primitives (projects, tasks, schedules, routes, hooks). xcb runs each task on one of your Claude or Codex accounts. The backlog and the attention queue carry questions and results back to the agent."
      caption={<>Agents coordinate through the backlog and the attention queue, so you check in instead of watching.</>}
    >
      <defs><DiagramArrowhead id="xcb-mechanism-arrow" /></defs>
      <g className="xcb-mechanism__box">
        <rect x={agentBox.x} y={agentBox.y} width={agentBox.w} height={agentBox.h} rx={8} />
        <text x={agentBox.x + agentBox.w / 2} y={agentBox.y + 40} textAnchor="middle">Your agent</text>
        <text className="xcb-mechanism__small" x={agentBox.x + agentBox.w / 2} y={agentBox.y + 62} textAnchor="middle">Claude Code, Codex,</text>
        <text className="xcb-mechanism__small" x={agentBox.x + agentBox.w / 2} y={agentBox.y + 80} textAnchor="middle">or your own app</text>
      </g>
      <g className="xcb-mechanism__box xcb-mechanism__box--hub">
        <rect x={hubBox.x} y={hubBox.y} width={hubBox.w} height={hubBox.h} rx={10} />
        <text x={hubBox.x + hubBox.w / 2} y={hubBox.y + 32} textAnchor="middle">xcb primitives</text>
        {primitives.map((name, index) => (
          <g key={name} className="xcb-mechanism__primitive">
            <rect x={hubBox.x + 20} y={hubBox.y + 50 + index * 34} width={hubBox.w - 40} height={26} rx={5} />
            <text x={hubBox.x + 32} y={hubBox.y + 68 + index * 34}>{`xcb ${name}`}</text>
          </g>
        ))}
        <text className="xcb-mechanism__small" x={hubBox.x + hubBox.w / 2} y={hubBox.y + hubBox.h - 14} textAnchor="middle">backlog · attention</text>
      </g>
      <g className="xcb-mechanism__box">
        <rect x={accountBox.x} y={accountBox.y} width={accountBox.w} height={accountBox.h} rx={8} />
        <text x={accountBox.x + accountBox.w / 2} y={accountBox.y + 40} textAnchor="middle">Your accounts</text>
        <text className="xcb-mechanism__small" x={accountBox.x + accountBox.w / 2} y={accountBox.y + 62} textAnchor="middle">Claude and Codex</text>
        <text className="xcb-mechanism__small" x={accountBox.x + accountBox.w / 2} y={accountBox.y + 80} textAnchor="middle">subscriptions</text>
      </g>
      <g className="xcb-mechanism__arrows">
        <path d={`M${agentBox.x + agentBox.w} 130 H${hubBox.x - 4}`} markerEnd="url(#xcb-mechanism-arrow)" />
        <text className="xcb-mechanism__small" x={(agentBox.x + agentBox.w + hubBox.x) / 2} y={120} textAnchor="middle">creates</text>
        <path d={`M${hubBox.x} 170 H${agentBox.x + agentBox.w + 4}`} markerEnd="url(#xcb-mechanism-arrow)" />
        <text className="xcb-mechanism__small" x={(agentBox.x + agentBox.w + hubBox.x) / 2} y={190} textAnchor="middle">questions, results</text>
        <path d={`M${hubBox.x + hubBox.w} 130 H${accountBox.x - 4}`} markerEnd="url(#xcb-mechanism-arrow)" />
        <text className="xcb-mechanism__small" x={(hubBox.x + hubBox.w + accountBox.x) / 2} y={120} textAnchor="middle">one task, one account</text>
        <path d={`M${accountBox.x} 170 H${hubBox.x + hubBox.w + 4}`} markerEnd="url(#xcb-mechanism-arrow)" />
        <text className="xcb-mechanism__small" x={(hubBox.x + hubBox.w + accountBox.x) / 2} y={190} textAnchor="middle">quota, limits</text>
      </g>
    </MarketingDiagram>
  );
}

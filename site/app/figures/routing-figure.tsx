/**
 * The routing figure: tasks fan out across a pool of subscription accounts.
 * One account reports a usage limit, dims, and the route it held shifts to
 * an account that can take the task. Motion is CSS only; with reduced motion
 * the figure shows the final frame.
 *
 * Account names are invented. The states are the ones xcb reports: an account
 * is routed to when it is signed in, idle, and not at a known quota limit.
 */
const accounts = [
  { id: "work", provider: "Claude", name: "Claude Max · work", state: "idle" },
  { id: "personal", provider: "Claude", name: "Claude Max · personal", state: "usage limit · resets 14:00" },
  { id: "codex-pro", provider: "Codex", name: "Codex Pro", state: "idle" },
  { id: "codex-plus", provider: "Codex", name: "Codex Plus", state: "idle" },
] as const;

const tasks = ["t_parser", "t_docs", "t_release"] as const;

const width = 640;
const height = 300;
const taskX = 24;
const hubX = 268;
const accountX = 404;
const accountWidth = 212;
const rowHeight = 64;
const firstRow = 44;

function accountY(index: number): number {
  return firstRow + index * rowHeight;
}

function taskY(index: number): number {
  return 70 + index * 76;
}

export function RoutingFigure() {
  const hubY = height / 2;
  return (
    <figure className="xcb-routing-figure" aria-labelledby="routing-figure-caption">
      <svg className="xcb-routing-figure__canvas" viewBox={`0 0 ${width} ${height}`} role="img" aria-labelledby="routing-figure-title">
        <title id="routing-figure-title">Three tasks reach xcb. xcb sends each to one of four accounts. One Claude account reports a usage limit, so its task moves to a Codex account.</title>
        {/* tasks */}
        {tasks.map((task, index) => (
          <g key={task} className="xcb-routing-figure__task">
            <rect x={taskX} y={taskY(index) - 18} width={132} height={36} rx={6} />
            <text x={taskX + 14} y={taskY(index) + 5}>{task}</text>
            <path d={`M${taskX + 132} ${taskY(index)} C ${taskX + 190} ${taskY(index)}, ${hubX - 60} ${hubY}, ${hubX} ${hubY}`} />
          </g>
        ))}
        {/* hub */}
        <g className="xcb-routing-figure__hub">
          <rect x={hubX} y={hubY - 30} width={88} height={60} rx={8} />
          <text x={hubX + 44} y={hubY - 4} textAnchor="middle">xcb</text>
          <text className="xcb-routing-figure__small" x={hubX + 44} y={hubY + 16} textAnchor="middle">route</text>
        </g>
        {/* routes: task 1 → work, task 2 → personal then codex-pro, task 3 → codex-plus */}
        <g className="xcb-routing-figure__routes">
          <path className="xcb-routing-figure__route" d={`M${hubX + 88} ${hubY} C ${hubX + 130} ${hubY}, ${accountX - 50} ${accountY(0)}, ${accountX} ${accountY(0)}`} />
          <path className="xcb-routing-figure__route xcb-routing-figure__route--dropped" d={`M${hubX + 88} ${hubY} C ${hubX + 130} ${hubY}, ${accountX - 50} ${accountY(1)}, ${accountX} ${accountY(1)}`} />
          <path className="xcb-routing-figure__route xcb-routing-figure__route--shifted" d={`M${hubX + 88} ${hubY} C ${hubX + 130} ${hubY}, ${accountX - 50} ${accountY(2)}, ${accountX} ${accountY(2)}`} />
          <path className="xcb-routing-figure__route" d={`M${hubX + 88} ${hubY} C ${hubX + 130} ${hubY}, ${accountX - 50} ${accountY(3)}, ${accountX} ${accountY(3)}`} />
        </g>
        {/* accounts */}
        {accounts.map((account, index) => (
          <g key={account.id} className={`xcb-routing-figure__account${account.state === "idle" ? "" : " xcb-routing-figure__account--limited"}`}>
            <rect x={accountX} y={accountY(index) - 22} width={accountWidth} height={44} rx={6} />
            <text x={accountX + 14} y={accountY(index) - 3}>{account.name}</text>
            <text className="xcb-routing-figure__small" x={accountX + 14} y={accountY(index) + 14}>{account.state}</text>
          </g>
        ))}
      </svg>
      <figcaption id="routing-figure-caption">When an account reports a usage limit, its task moves to another account with its original instructions.</figcaption>
    </figure>
  );
}

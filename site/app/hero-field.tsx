import type { CSSProperties } from "react";

type FieldNote = {
  id: string;
  type: "agent" | "route" | "session" | "lock" | "limit" | "terminal" | "record" | "task" | "rule";
  title: string;
  body: string;
  x: number;
  y: number;
  rotate: number;
  width: number;
  tags?: string[];
};

const notes: FieldNote[] = [
  { id: "devin", type: "agent", title: "devin", body: "Supported build on macOS. Ranked below the pick.", x: 8, y: 26, rotate: 1.4, width: 178 },
  { id: "codex", type: "agent", title: "codex · plus", body: "Signed in and idle. gpt-5.2-codex seen moments ago.", tags: ["eligible"], x: 45, y: 11, rotate: -1.2, width: 186 },
  { id: "claude", type: "agent", title: "claude · max", body: "Signed in. Inside a provider-reported quota window.", tags: ["resumes at reset"], x: 83, y: 13, rotate: 1.7, width: 172 },
  { id: "task", type: "task", title: "fix the failing parser test", body: "Arrives as a task. Carries no credentials or hooks.", x: 3, y: 55, rotate: -1.6, width: 180 },
  { id: "route", type: "route", title: "route → codex/gpt-5.2-codex", body: "One turn on one model. The returned text is capped.", x: 31, y: 39, rotate: -0.8, width: 198 },
  { id: "session", type: "session", title: "session s_4f2a", body: "Resumable. Processes exited; outcome recorded.", x: 61, y: 50, rotate: 0.9, width: 188 },
  { id: "lease", type: "lock", title: "account held · a7f2", body: "One task at a time. Released after the provider process exits.", x: 91, y: 62, rotate: -1.1, width: 192 },
  { id: "quota", type: "limit", title: "quota window", body: "The account sits out until the provider’s reported reset.", x: 95, y: 36, rotate: 0.6, width: 176 },
  { id: "terminal", type: "terminal", title: "$ xcb --json route", body: "stdin: one JSON task\nstdout: one JSON result", x: 15, y: 79, rotate: -2, width: 204 },
  { id: "receipt", type: "record", title: "record verified", body: "Replays the task’s local record to check it.", x: 49, y: 85, rotate: 1.5, width: 182 },
  { id: "manifest", type: "rule", title: "routing rule", body: "In development. Kept only if it scores strictly better.", x: 77, y: 86, rotate: -2.2, width: 176 },
];

const edges = [
  { from: "task", to: "route", label: "submits", pulse: true },
  { from: "route", to: "codex", label: "holds account" },
  { from: "route", to: "session", label: "records" },
  { from: "codex", to: "session", label: "runs", pulse: true },
  { from: "devin", to: "route", label: "candidate" },
  { from: "claude", to: "quota", label: "inside" },
  { from: "quota", to: "route", label: "fails over" },
  { from: "session", to: "lease", label: "released after" },
  { from: "terminal", to: "task", label: "drives" },
  { from: "receipt", to: "session", label: "replays" },
  { from: "manifest", to: "route", label: "proposes", pulse: true },
  { from: "terminal", to: "receipt", label: "verifies" },
];

const noteById = new Map(notes.map((note) => [note.id, note]));

function edgePath(from: FieldNote, to: FieldNote) {
  const midX = (from.x + to.x) / 2;
  const bend = Math.min(10, 0.5 * Math.abs(from.y - to.y) + 5);
  const topY = Math.min(from.y, to.y) - bend;
  return `M ${from.x} ${from.y} Q ${midX} ${topY} ${to.x} ${to.y}`;
}

function edgeMid(from: FieldNote, to: FieldNote) {
  const midX = (from.x + to.x) / 2;
  const bend = Math.min(10, 0.5 * Math.abs(from.y - to.y) + 5);
  const topY = Math.min(from.y, to.y) - bend;
  return [(from.x + 2 * midX + to.x) / 4, (from.y + 2 * topY + to.y) / 4] as const;
}

/** Product-owned routing artwork. The shared HeroBackdrop owns pointer light,
 * proximity, offscreen suspension, and reduced-motion behavior. */
export function HeroField() {
  return (
    <div aria-hidden="true" className="xcb-field">
      <svg className="xcb-edges" preserveAspectRatio="none" viewBox="0 0 100 100">
        {edges.map((edge) => {
          const from = noteById.get(edge.from);
          const to = noteById.get(edge.to);
          if (from === undefined || to === undefined) return null;
          return <path key={`${edge.from}-${edge.to}`} className={edge.pulse === true ? "xcb-edge xcb-edge--pulse" : "xcb-edge"} d={edgePath(from, to)} data-hraness-hero-item="" />;
        })}
      </svg>
      {edges.map((edge) => {
        const from = noteById.get(edge.from);
        const to = noteById.get(edge.to);
        if (from === undefined || to === undefined) return null;
        const [x, y] = edgeMid(from, to);
        return <span key={`label-${edge.from}-${edge.to}`} className="xcb-edge-label" data-hraness-hero-item="" style={{ left: `${x}%`, top: `${y}%` }}>{edge.label}</span>;
      })}
      {notes.map((note) => (
        <article
          key={note.id}
          className="xcb-field-note"
          data-hraness-hero-item=""
          data-type={note.type}
          style={{
            "--x": `${note.x}%`, "--y": `${note.y}%`, "--w": `${note.width}px`, "--r": `${note.rotate}deg`,
          } as CSSProperties}
        >
          <span className="xcb-field-note-type">{note.type}</span>
          <p className="xcb-field-note-title">{note.title}</p>
          <p className="xcb-field-note-body">{note.body}</p>
          {note.tags !== undefined && <div className="xcb-field-note-tags">{note.tags.map((tag) => <span key={tag}>#{tag}</span>)}</div>}
        </article>
      ))}
    </div>
  );
}

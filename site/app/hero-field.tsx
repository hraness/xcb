import type { CSSProperties } from "react";

type FieldNote = {
  id: string;
  type: "agent" | "route" | "session" | "custody" | "limit" | "terminal" | "receipt" | "task" | "manifest";
  title: string;
  body: string;
  x: number;
  y: number;
  rotate: number;
  width: number;
  tags?: string[];
};

const notes: FieldNote[] = [
  { id: "claude", type: "agent", title: "claude · max", body: "Enabled, credentialed, idle. sonnet 4.6 observed moments ago.", tags: ["eligible"], x: 13, y: 26, rotate: 1.4, width: 178 },
  { id: "codex", type: "agent", title: "codex · plus", body: "gpt-5.2-codex observed. One Pareto tier behind the pick.", x: 46, y: 11, rotate: -1.2, width: 186 },
  { id: "devin", type: "agent", title: "devin", body: "ACP candidate. Inside a provider-reported quota window.", tags: ["resumes at reset"], x: 79, y: 13, rotate: 1.7, width: 172 },
  { id: "task", type: "task", title: "fix the failing parser test", body: "Arrives as one bounded task — never a credential, never a hook.", x: 9, y: 55, rotate: -1.6, width: 180 },
  { id: "route", type: "route", title: "route → claude/sonnet", body: "One turn. Explicit model, deadline, and output bound.", x: 34, y: 39, rotate: -0.8, width: 198 },
  { id: "session", type: "session", title: "session s_4f2a", body: "Resumable. Processes joined; outcome facts recorded.", x: 60, y: 50, rotate: 0.9, width: 188 },
  { id: "lease", type: "custody", title: "lease held · a7f2", body: "Exclusive, generation-fenced. Released only after exit is proven.", x: 86, y: 62, rotate: -1.1, width: 192 },
  { id: "quota", type: "limit", title: "quota window", body: "A provider-reported reset excludes that account's routes.", x: 90, y: 36, rotate: 0.6, width: 176 },
  { id: "terminal", type: "terminal", title: "$ xcb --json route", body: "stdin: one task document\nstdout: one bounded result", x: 20, y: 79, rotate: -2, width: 204 },
  { id: "receipt", type: "receipt", title: "receipt verified", body: "The settled outcome replays against the written record.", x: 49, y: 85, rotate: 1.5, width: 182 },
  { id: "manifest", type: "manifest", title: "routing manifest", body: "A proposed router. Promoted only when strictly better.", x: 74, y: 86, rotate: -2.2, width: 176 },
];

const edges = [
  { from: "task", to: "route", label: "submits", pulse: true },
  { from: "route", to: "claude", label: "holds lease" },
  { from: "route", to: "session", label: "settles" },
  { from: "claude", to: "session", label: "runs", pulse: true },
  { from: "codex", to: "route", label: "candidate" },
  { from: "devin", to: "quota", label: "inside" },
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
          <h3 className="xcb-field-note-title">{note.title}</h3>
          <p className="xcb-field-note-body">{note.body}</p>
          {note.tags !== undefined && <div className="xcb-field-note-tags">{note.tags.map((tag) => <span key={tag}>#{tag}</span>)}</div>}
        </article>
      ))}
    </div>
  );
}

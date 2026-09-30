import { MockupRoot, SampleText, TerminalFrame, WindowLights, type MockupTheme, type TerminalLine } from "@hraness/design-kit/mockups";

import {
  attentionRows,
  boardAccounts,
  boardNote,
  boardPick,
  dispatchCommand,
  dispatchResult,
  fleetRows,
  routeRequest,
  routeResponse,
  taskRows,
  threadLines,
  type RouterMode,
} from "./fixtures";
import { installCommand as installCommandLine } from "../install/commands";

type SurfaceProps = Readonly<{ mode: RouterMode; theme?: MockupTheme; height?: number }>;

const PROVIDER_NAMES = { claude: "Claude", codex: "Codex", devin: "Devin" } as const;

/**
 * The `xcb accounts` table with a meter drawn beside each status cell. The
 * status text is what the CLI prints; the bars and the "picked" chip are
 * drawn for the illustration.
 */
export function AccountsBoard({ height, mode, theme }: SurfaceProps) {
  const rows = boardAccounts[mode];
  const pick = boardPick[mode];
  const picked = rows.find((row) => row.id === pick);
  const describe = mode === "reset"
    ? `Illustration of the xcb accounts list: four made-up accounts with how much quota each has left. The ${picked?.name ?? ""} Claude account resets soonest and takes the task.`
    : `Illustration of the xcb accounts list: the ${rows[0]?.name ?? ""} Claude account has hit its limit, and the task moves to the ${picked?.name ?? ""} Claude account.`;
  return (
    <MockupRoot describe={describe} kind="xcb-board" theme={theme} className="xcb-mock">
      <div className="hkm-window">
        <div aria-hidden="true" className="hkm-title-bar">
          <WindowLights />
          <span className="hkm-title">xcb accounts</span>
          <span />
        </div>
        <div className="xcb-mock-body" style={height === undefined ? undefined : { minHeight: height }}>
          <div className="xcb-board-head" aria-hidden="true">
            <span>Account</span>
            <span>Plan</span>
            <span>Quota left</span>
          </div>
          <ul className="xcb-board-rows">
            {rows.map((row) => {
              const isPick = row.id === pick;
              const tone = row.status.startsWith("limited") ? "limit" : isPick ? "pick" : "idle";
              return (
                <li className="xcb-board-row" data-tone={tone} data-hkm-beat={isPick ? "pick" : undefined} key={row.id}>
                  <span className="xcb-board-account">
                    <span className="xcb-board-provider" data-provider={row.provider}>{PROVIDER_NAMES[row.provider]}</span>
                    <SampleText>{row.name}</SampleText>
                    {isPick ? <span className="xcb-board-chip">Gets the task</span> : null}
                  </span>
                  <span className="xcb-board-plan"><SampleText>{row.plan}</SampleText></span>
                  <span className="xcb-board-usage">
                    <span className="xcb-board-meter" aria-hidden="true" data-empty={row.left === null ? "" : undefined}>
                      <span style={{ inlineSize: `${row.left ?? 0}%` }} />
                    </span>
                    <span className="xcb-board-status"><SampleText>{row.status}</SampleText></span>
                  </span>
                </li>
              );
            })}
          </ul>
          <p className="xcb-board-note"><SampleText>{boardNote[mode]}</SampleText></p>
        </div>
      </div>
    </MockupRoot>
  );
}

/** Your one thread in the xcb terminal app: the task you typed and what xcb shows back. */
export function ThreadView({ height, mode, theme }: SurfaceProps) {
  const lines: TerminalLine[] = threadLines[mode].map((line, index) => ({
    kind: line.who === "you" ? "input" : "output",
    text: line.text,
    ...(line.tone === undefined ? {} : { tone: line.tone }),
    beat: `thread-${index}`,
  }));
  const describe = mode === "reset"
    ? "Illustration of an xcb thread: you type a task, xcb names the project it picked, runs the task on a Claude account, and shows the answer."
    : "Illustration of an xcb thread: the task stops at a usage limit on one Claude account, continues on another, and finishes.";
  return <TerminalFrame describe={describe} height={height} lines={lines} prompt="›" theme={theme} title="xcb" />;
}

/** `xcb tasks` and `xcb attention` in one terminal. */
export function TasksView({ height, mode, theme }: SurfaceProps) {
  const lines: TerminalLine[] = [
    { kind: "input", text: "xcb tasks" },
    ...taskRows[mode].map((text, index): TerminalLine => ({ kind: "output", text, tone: text.includes("needs input") ? "warn" : text.includes("completed") ? "ok" : undefined, beat: `task-${index}` })),
    { kind: "input", text: "xcb attention" },
    ...attentionRows.map((text): TerminalLine => ({ kind: "output", text, tone: text.startsWith(" ") ? "muted" : "warn", beat: "attention" })),
  ];
  return (
    <TerminalFrame
      describe="Illustration of xcb tasks and xcb attention: three made-up tasks, one running, one waiting for your answer, one done, and the question that needs you."
      height={height}
      lines={lines}
      theme={theme}
      title="Terminal"
    />
  );
}

/** `xcb fleet` and `xcb dispatch` from a laptop. */
export function FleetView({ height, theme }: Omit<SurfaceProps, "mode">) {
  const lines: TerminalLine[] = [
    { kind: "input", text: "xcb fleet" },
    ...fleetRows.map((text): TerminalLine => ({ kind: "output", text, tone: text.includes("STALE") || text.includes("offline") ? "warn" : text.startsWith(" ") ? "muted" : undefined, beat: "fleet" })),
    { kind: "input", text: dispatchCommand, beat: "dispatch" },
    { kind: "output", text: dispatchResult, tone: "ok", beat: "dispatch" },
  ];
  return (
    <TerminalFrame
      describe="Illustration of xcb fleet on a laptop: three made-up linked machines, one offline, and a task sent to the desktop at home."
      height={height}
      lines={lines}
      theme={theme}
      title="travel-laptop"
    />
  );
}

/** A route request and its JSON result, side by side. */
export function RouteSplit({ theme }: Readonly<{ theme?: MockupTheme }>) {
  return (
    <MockupRoot
      describe="Illustration of xcb --json route: an app sends a task as JSON and gets back which account ran it, how it ended, and the answer."
      kind="xcb-route"
      theme={theme}
      className="xcb-mock"
    >
      <div className="xcb-route-split">
        {[
          { title: "task.json", code: routeRequest, beat: "request" },
          { title: "xcb --json route < task.json", code: routeResponse, beat: "result" },
        ].map((pane) => (
          <div className="hkm-window" data-hkm-beat={pane.beat} key={pane.title}>
            <div aria-hidden="true" className="hkm-title-bar">
              <WindowLights />
              <span className="hkm-title">{pane.title}</span>
              <span />
            </div>
            <div className="xcb-route-code"><SampleText>{pane.code}</SampleText></div>
          </div>
        ))}
      </div>
    </MockupRoot>
  );
}

/** What stands between a provider and your project on every run, from README "How xcb runs a task". */
const RUN_LAYERS = [
  { id: "signin", icon: "key", label: "Your sign-in", detail: "Kept in a private profile outside your projects" },
  { id: "provider", icon: "cli", label: "The provider's own tool", detail: "A private copy of Claude Code, Codex, or the Devin CLI" },
  { id: "sandbox", icon: "shield", label: "An OS sandbox", detail: "Seatbelt on macOS, bwrap on Linux, a cleared environment" },
  { id: "tools", icon: "file", label: "xcb's file tools", detail: "The only way in to your files; no provider plugins or MCP servers" },
  { id: "project", icon: "folder", label: "One project folder", detail: "Changes land here for you to review and commit" },
] as const;

type RunFocus = "all" | "limits";

function RunIcon({ name }: Readonly<{ name: (typeof RUN_LAYERS)[number]["icon"] }>) {
  const common = { fill: "none", stroke: "currentColor", strokeWidth: 1.6, strokeLinecap: "round", strokeLinejoin: "round" } as const;
  switch (name) {
    case "key":
      return <svg aria-hidden="true" viewBox="0 0 24 24" width="20" height="20"><circle cx="8" cy="12" r="4" {...common} /><path d="M12 12h9m-3 0v3m-3-3v2" {...common} /></svg>;
    case "cli":
      return <svg aria-hidden="true" viewBox="0 0 24 24" width="20" height="20"><rect x="3" y="4" width="18" height="16" rx="2" {...common} /><path d="m7 9 3 3-3 3m5 0h5" {...common} /></svg>;
    case "shield":
      return <svg aria-hidden="true" viewBox="0 0 24 24" width="20" height="20"><path d="M12 3 5 6v6c0 4 3 7 7 9 4-2 7-5 7-9V6z" {...common} /></svg>;
    case "file":
      return <svg aria-hidden="true" viewBox="0 0 24 24" width="20" height="20"><path d="M6 3h8l4 4v14H6z M14 3v4h4 M9 13h6 M9 17h6" {...common} /></svg>;
    case "folder":
      return <svg aria-hidden="true" viewBox="0 0 24 24" width="20" height="20"><path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" {...common} /></svg>;
  }
}

/**
 * How one task runs, drawn as nested layers from your sign-in to your
 * project. With `focus="limits"` the file-tools layer is marked, because it
 * is where xcb runs differ from the provider's own CLI.
 */
export function RunDiagram({ focus = "all", theme }: Readonly<{ focus?: RunFocus; theme?: MockupTheme }>) {
  const describe = focus === "limits"
    ? "Illustration of how xcb runs a task, with the file tools layer marked: providers reach your files only through xcb's own tools."
    : "Illustration of how xcb runs a task: your sign-in, the provider's own tool, an OS sandbox, xcb's file tools, and one project folder.";
  return (
    <MockupRoot describe={describe} kind="xcb-run" theme={theme} className="xcb-mock">
      <ol className="xcb-run" data-focus={focus}>
        {RUN_LAYERS.map((layer, index) => (
          <li className="xcb-run-layer" data-hkm-beat={`layer-${index}`} data-layer={layer.id} key={layer.id}>
            <span className="xcb-run-icon"><RunIcon name={layer.icon} /></span>
            <span className="xcb-run-text">
              <strong>{layer.label}</strong>
              <span>{layer.detail}</span>
            </span>
          </li>
        ))}
      </ol>
    </MockupRoot>
  );
}

/** The three commands that take you from nothing to your thread. */
export function InstallView({ height, theme }: Readonly<{ height?: number; theme?: MockupTheme }>) {
  const lines: TerminalLine[] = [
    { kind: "input", text: installCommandLine, beat: "install" },
    { kind: "input", text: "xcb setup claude", beat: "setup" },
    { kind: "input", text: "xcb", beat: "open" },
  ];
  return (
    <TerminalFrame
      describe="Illustration of installing xcb: the one-line installer, connecting a Claude account, and opening your thread."
      height={height}
      lines={lines}
      theme={theme}
      title="Terminal"
    />
  );
}

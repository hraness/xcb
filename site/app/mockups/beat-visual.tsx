import type { LaunchBeat } from "@hraness/design-kit/launch";

import type { RouterMode } from "./fixtures";
import { AccountsBoard, FleetView, InstallView, RouteSplit, RunDiagram, TasksView, ThreadView } from "./surfaces";

/** The mockup ids a launch beat can name, each drawn by one component. */
export const beatMockupIds = ["thread", "accounts", "tasks", "fleet", "run", "route", "install"] as const;
export type BeatMockupId = (typeof beatMockupIds)[number];

function modeOf(state: Readonly<Record<string, string>>): RouterMode {
  return state["mode"] === "limit" ? "limit" : "reset";
}

/** The one visual for a launch beat. Every beat in this post shows a code-built illustration. */
export function BeatVisual({ beat }: Readonly<{ beat: LaunchBeat }>) {
  const visual = beat.visual;
  if (visual.kind !== "mockup") throw new Error(`Beat ${beat.id} names a ${visual.kind}; this post shows illustrations only.`);
  const id = visual.id as BeatMockupId;
  switch (id) {
    case "thread":
      return <ThreadView height={260} mode={modeOf(visual.state)} />;
    case "accounts":
      return <AccountsBoard mode={modeOf(visual.state)} />;
    case "tasks":
      return <TasksView height={250} mode={modeOf(visual.state)} />;
    case "fleet":
      return <FleetView height={240} />;
    case "run":
      return <RunDiagram focus={visual.state["focus"] === "limits" ? "limits" : "all"} />;
    case "route":
      return <RouteSplit />;
    case "install":
      return <InstallView height={150} />;
  }
  throw new Error(`Beat ${beat.id} names an unknown mockup ${JSON.stringify(visual.id)}.`);
}

"use client";

import { ModeShowcase, type ShowcaseChoice, type ShowcaseSurface } from "@hraness/design-kit/mockups/client";

import type { RouterMode } from "./fixtures";
import { AccountsBoard, TasksView, ThreadView } from "./surfaces";

type Surface = "accounts" | "thread" | "tasks";

const surfaces: readonly ShowcaseSurface<Surface, RouterMode>[] = [
  { id: "accounts", label: "Your accounts", render: ({ mode, theme }) => <AccountsBoard mode={mode} theme={theme} /> },
  { id: "thread", label: "Your thread", render: ({ mode, theme }) => <ThreadView height={300} mode={mode} theme={theme} /> },
  { id: "tasks", label: "Tasks", render: ({ mode, theme }) => <TasksView height={300} mode={mode} theme={theme} /> },
];

const modes: readonly ShowcaseChoice<RouterMode>[] = [
  { id: "reset", label: "Quota about to reset" },
  { id: "limit", label: "Account hits its limit" },
];

/**
 * The interactive router illustration on the home page and in the launch
 * post: tabs pick a surface, the mode switch flips between a normal pick and
 * a limit hit mid-task.
 */
export function RouterShowcase({ className }: Readonly<{ className?: string }> = {}) {
  return (
    <ModeShowcase
      className={className}
      height={340}
      label={(surface) => `Illustration of xcb: ${surface.label.toLowerCase()}`}
      minWidth={560}
      modeLabel="Show"
      modes={modes}
      surfaces={surfaces}
    />
  );
}

"use client";

import { useRef, useState } from "react";

import {
  ModeShowcase,
  type ShowcaseChoice,
  type ShowcaseSurface,
} from "@hraness/design-kit/mockups/client";

import type { RouterMode } from "./fixtures";
import { AccountsBoard, TasksView, ThreadView } from "./surfaces";

type Surface = "accounts" | "thread" | "tasks";

const surfaces: readonly ShowcaseSurface<Surface, RouterMode>[] = [
  {
    id: "accounts",
    label: "Accounts at a glance",
    render: ({ mode, theme }) => <AccountsBoard mode={mode} theme={theme} />,
  },
  {
    id: "thread",
    label: "A task in motion",
    render: ({ mode, theme }) => (
      <ThreadView height={300} mode={mode} theme={theme} />
    ),
  },
  {
    id: "tasks",
    label: "Work to follow up",
    render: ({ mode, theme }) => (
      <TasksView height={300} mode={mode} theme={theme} />
    ),
  },
];

const modes: readonly ShowcaseChoice<RouterMode>[] = [
  { id: "reset", label: "Room to work" },
  { id: "limit", label: "A limit appears" },
];

/**
 * The interactive router illustration on the home page and in the launch
 * post: tabs pick a surface, the mode switch flips between a normal pick and
 * a limit hit mid-task.
 */
export function RouterShowcase({
  className,
}: Readonly<{ className?: string }> = {}) {
  const shell = useRef<HTMLDivElement>(null);
  const [surfaceIndex, setSurfaceIndex] = useState(0);
  const select = (delta: number) => {
    const tabs = shell.current?.querySelectorAll<HTMLButtonElement>(".hkm-tab");
    if (!tabs?.length) return;
    const next = (surfaceIndex + delta + tabs.length) % tabs.length;
    tabs[next]?.click();
    setSurfaceIndex(next);
  };
  return (
    <div
      ref={shell}
      className="xcb-showcase-shell"
      onClickCapture={(event) => {
        const tab = (event.target as HTMLElement).closest<HTMLButtonElement>(
          ".hkm-tab",
        );
        if (tab?.parentElement)
          setSurfaceIndex([...tab.parentElement.children].indexOf(tab));
      }}
    >
      <ModeShowcase
        className={className}
        height={340}
        label={(surface) =>
          `Illustration of xcb: ${surface.label.toLowerCase()}`
        }
        fit="fill"
        modeLabel="Show"
        modes={modes}
        surfaces={surfaces}
      />
      <div className="xcb-showcase-nav" aria-label="Preview navigation">
        <button
          type="button"
          aria-label="Previous xcb illustration"
          onClick={() => select(-1)}
        >
          ‹
        </button>
        <span>
          {String(surfaceIndex + 1).padStart(2, "0")} /{" "}
          {String(surfaces.length).padStart(2, "0")}
        </span>
        <button
          type="button"
          aria-label="Next xcb illustration"
          onClick={() => select(1)}
        >
          ›
        </button>
      </div>
    </div>
  );
}

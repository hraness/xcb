/**
 * The film's product surface, built from the site's own mockups
 * (site/app/mockups), so the film shows what the home page and the launch
 * post show. The camera frames each `data-film` block; cursor targets inside
 * a mockup use the kit's `data-hkm-beat` hooks.
 *
 * Illustration only: accounts, projects, and tasks are made up.
 */
import type { ReactNode } from "react";

import { AccountsBoard, FleetView, RouteSplit, TasksView, ThreadView } from "../site/app/mockups/surfaces.tsx";

function Block({ film, label, children }: Readonly<{ film: string; label: string; children: ReactNode }>) {
  return (
    <div data-film={film}>
      <p className="xcb-desk-label">{label}</p>
      {children}
    </div>
  );
}

/** Six surfaces on one desk: two threads, the accounts list, tasks, fleet, and route. */
export function ProductMockup() {
  return (
    <div className="xcb-desk" role="img" aria-label="Illustration of xcb: a thread, the accounts list, background tasks, linked machines, and a JSON route.">
      <Block film="thread" label="Your thread"><ThreadView height={300} mode="reset" theme="light" /></Block>
      <Block film="accounts" label="xcb accounts"><AccountsBoard mode="reset" theme="light" /></Block>
      <Block film="thread-limit" label="At a limit"><ThreadView height={300} mode="limit" theme="light" /></Block>
      <Block film="tasks" label="xcb tasks"><TasksView height={280} mode="limit" theme="light" /></Block>
      <Block film="fleet" label="xcb fleet"><FleetView height={280} theme="light" /></Block>
      <Block film="route" label="For agents"><RouteSplit theme="light" /></Block>
    </div>
  );
}

/** The cold open: the moment one plan runs out, on made-up accounts. */
const OPEN_CARDS = [
  { account: "work-claude", text: "Usage limit reached. Resets in 3 hours." },
  { account: "codex-personal", text: "Weekly limit reached. Resets Monday." },
  { account: "side-claude", text: "Usage limit reached. Resets in 47 minutes." },
  { account: "team-codex", text: "Rate limited. Try again later." },
  { account: "devin-work", text: "Out of session quota for today." },
  { account: "home-claude", text: "Usage limit reached. Resets at 6 pm." },
] as const;

export function OpenCard({ index }: Readonly<{ index: number }>) {
  const card = OPEN_CARDS[index % OPEN_CARDS.length]!;
  return (
    <div className="fm-card">
      <span className="fm-card-dot" aria-hidden="true" />
      <div>
        <b>{card.account}</b>
        <p>{card.text}</p>
      </div>
    </div>
  );
}

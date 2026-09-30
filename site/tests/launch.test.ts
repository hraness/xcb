import { describe, expect, test } from "bun:test";
import { readFile } from "node:fs/promises";
import { join } from "node:path";

import { launchBeats, launchPostSlug, socialKit } from "../app/launch/beats";
import { LAUNCH_STATUS, launchFacts, launchStatusFor } from "../app/launch/facts";
import {
  attentionRows,
  boardAccounts,
  dispatchResult,
  fleetRows,
  routeResponse,
  taskRows,
  threadLines,
} from "../app/mockups/fixtures";
import { blogPosts, findBlogPost } from "../app/blog/posts";
import { assertArticleVideo } from "@hraness/design-kit";
import { docsTopics } from "../app/docs/topics";
import { launchFilm } from "../app/launch/film";
import { renderSocialKitMarkdown } from "../app/launch/social-kit-markdown";
import { publishedRelease } from "../app/publication";

const repo = join(import.meta.dir, "../..");
const read = (path: string) => readFile(join(repo, path), "utf8");

/** Facts are pinned to the records they come from, not to the prose around them. */
describe("xcb launch facts", () => {
  test("each number matches its source record", async () => {
    const [readme, quota] = await Promise.all([read("README.md"), read("docs/quota-routing.md")]);
    expect(launchFacts.providers.value).toBe("three");
    for (const provider of ["Claude", "Codex", "Devin"]) expect(readme).toContain(provider);
    expect(quota).toContain("at most five minutes old");
    expect(launchFacts.meterMaxAge.value).toBe("five minutes");
    expect(quota).toMatch(/Claude uses `five_hour` and `seven_day`/u);
    expect(launchFacts.claudeWindows.value).toBe("5-hour and 7-day");
    expect(readme).toContain("active in the last 24 hours");
    expect(launchFacts.importWindow.value).toBe("24 hours");
  });

  test("the status comes from the published release record", () => {
    expect(launchStatusFor(null)).toBe("Preview");
    expect(launchStatusFor({ version: "1.2.3" } as never)).toBe("Latest release: v1.2.3");
    expect(LAUNCH_STATUS).toBe(launchStatusFor(publishedRelease));
    const status = launchBeats.find(({ id }) => id === "status");
    expect(status?.post).toContain(LAUNCH_STATUS);
  });

  test("the post is a quarantined beats post until reviewed", () => {
    const post = findBlogPost(launchPostSlug);
    expect(post?.format).toBe("beats");
    expect(post?.admission.review).toBeNull();
    expect(post?.admission.lifecycle).toBe("quarantined");
  });

  test("every beat has a visual and alt text, and the kit ends threads at the post", () => {
    expect(launchBeats.length).toBeGreaterThanOrEqual(8);
    for (const beat of launchBeats) {
      expect(beat.visual).toBeDefined();
      expect(beat.alt.trim().length).toBeGreaterThan(0);
    }
    expect(JSON.stringify(socialKit)).toContain(`https://xcb.sh/blog/${launchPostSlug}`);
    expect(JSON.stringify(socialKit)).not.toMatch(/mastodon/iu);
  });

  test("kb/launch/social-kit.md is the generated kit", async () => {
    expect(await read("kb/launch/social-kit.md")).toBe(renderSocialKitMarkdown());
  });
});

/** The illustrations copy CLI output shapes; a drift in the Rust source fails here. */
describe("xcb illustration shapes", () => {
  test("account status cells match xcb accounts", async () => {
    const health = await read("crates/xcb-cli/src/health.rs");
    expect(health).toContain('format!(" · {percent:.0}% left{reset}")');
    expect(health).toContain('format!(", resets in {}"');
    expect(health).toContain('format!("limited · retry in {}"');
    for (const account of Object.values(boardAccounts).flat()) {
      expect(account.status).toMatch(/^(ready|busy)( · \d+% left, resets in ~[\dhmd ]+)?$|^limited · retry in ~[\dhm ]+$/u);
    }
  });

  test("thread lines match the runtime's start, running, and limit notices", async () => {
    const managed = await read("crates/xcb-runtime/src/managed.rs");
    expect(managed).toContain('"Started **{}** in `{}` · {}{}"');
    expect(managed).toContain('" · /workspace to move"');
    expect(managed).toContain('"worker is running · {}"');
    expect(managed).toContain('"Usage limit interrupted {}; selecting another eligible route"');
    const limit = threadLines.limit.map(({ text }) => text);
    expect(limit.some((line) => /^Usage limit interrupted \S+ · a_\w+; selecting another eligible route$/u.test(line))).toBe(true);
    for (const line of Object.values(threadLines).flat().filter(({ who }) => who === "xcb")) {
      if (line.text.startsWith("Started ")) expect(line.text).toMatch(/ · \/workspace to move$/u);
    }
  });

  test("task, attention, fleet, and dispatch rows match the CLI", async () => {
    const [main, habitat, remote] = await Promise.all([
      read("crates/xcb-cli/src/main.rs"),
      read("crates/xcb-cli/src/habitat.rs"),
      read("crates/xcb-cli/src/remote.rs"),
    ]);
    expect(main).toContain('"{}  {} · {} · {}{}"');
    for (const row of Object.values(taskRows).flat()) expect(row).toMatch(/^t_\w+  [a-z ]+ · .+ · .+ · \w+\/[\w-]+ · a_\w+$/u);
    expect(habitat).toContain('"{} · {} · {} · {}"');
    expect(attentionRows[0]).toMatch(/^t_\w+ · \w+ · needs input · .+$/u);
    expect(remote).toContain('"{} · {} · {} · {} · {}"');
    expect(remote).toContain('"  projection {} · rev {} · {}s old{}"');
    expect(remote).toContain('" · STALE"');
    for (const row of fleetRows) {
      expect(row).toMatch(/^d_\w+ · [\w-]+ · (daemon|controller) · (online|offline) · \w+$|^ {2}projection \w+ · rev \d+ · \d+s old( · STALE)?$/u);
    }
    expect(remote).toContain('"Posted to {} as {} (idempotency {})."');
    expect(dispatchResult).toMatch(/^Posted to d_\w+ as c_\w+ \(idempotency k_\w+\)\.$/u);
  });

  test("the route result uses the documented fields", async () => {
    const route = await read("docs/route.md");
    const parsed = JSON.parse(routeResponse) as Record<string, unknown>;
    for (const key of Object.keys(parsed)) expect(route).toContain(`"${key}"`);
  });

  test("the launch film names only files that exist", async () => {
    if (launchFilm === null) return;
    assertArticleVideo(launchFilm.video);
    const { video } = launchFilm;
    for (const path of [...video.sources.map((source) => source.src), video.poster, video.captions]) {
      expect(await Bun.file(join(import.meta.dir, "..", "public", path)).exists()).toBe(true);
    }
  });

  test("every beat's internal detail link is a page that exists", async () => {
    const pages = new Set([...docsTopics.map((topic) => `/docs/${topic.slug}`), ...blogPosts.map((entry) => `/blog/${entry.slug}`)]);
    for (const beat of launchBeats) {
      const href = beat.detailHref;
      if (href === undefined || !href.startsWith("/")) continue;
      const route = join(import.meta.dir, "..", "app", href, "page.tsx");
      expect(pages.has(href) || (await Bun.file(route).exists())).toBe(true);
    }
  });
});

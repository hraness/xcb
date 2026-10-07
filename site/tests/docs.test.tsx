import { describe, expect, test } from "bun:test";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { renderToStaticMarkup } from "react-dom/server";
import Docs, { metadata as overviewMetadata } from "../app/docs/page";
import DocsTopicPage, { dynamicParams, generateMetadata, generateStaticParams } from "../app/docs/[slug]/page";
import { providerStatus, supportedBuilds } from "../app/docs/provider-status";
import { sdkExample, sdkExampleOutput } from "../app/docs/sdk-example";
import { docsGroups, docsTopics } from "../app/docs/topics";
import { publishedRelease } from "../app/publication";
import { headingText as textOf } from "../scripts/readme-html";

const repository = join(import.meta.dir, "..", "..");

async function renderTopic(slug: string): Promise<string> {
  return renderToStaticMarkup(await DocsTopicPage({ params: Promise.resolve({ slug }) }));
}

function attributeValues(html: string, selector: string, attribute: string): string[] {
  const values: string[] = [];
  new HTMLRewriter().on(selector, {
    element(element) { values.push(element.getAttribute(attribute) ?? ""); },
  }).transform(html);
  return values;
}

/** Rendered markup escapes apostrophes; compare sentences in that form. */
function rendered(text: string): string {
  return renderToStaticMarkup(<>{text}</>);
}

function occurrences(html: string, text: string): number {
  return html.split(text).length - 1;
}

describe("organized documentation", () => {
  test("prerenders every linked topic and does not enable arbitrary slugs", async () => {
    expect(dynamicParams).toBe(false);
    expect(generateStaticParams()).toEqual([
      { slug: "getting-started" }, { slug: "how-routing-works" }, { slug: "security" },
      { slug: "projects-and-tasks" }, { slug: "providers" }, { slug: "workspace" }, { slug: "customization" },
      { slug: "reflexes" }, { slug: "upgrade-and-uninstall" }, { slug: "troubleshooting" },
      { slug: "route" }, { slug: "sdk" }, { slug: "application-api" }, { slug: "reference" },
    ]);
    const groups = new Set<string>(docsGroups.map((group) => group.name));
    const overview = renderToStaticMarkup(<Docs />);
    for (const topic of docsTopics) {
      expect(groups.has(topic.group)).toBe(true);
      expect(overview).toContain(`href="/docs/${topic.slug}"`);
      const html = await renderTopic(topic.slug);
      expect(html.match(/<h1\b/gu)).toHaveLength(1);
      expect(html.match(/<main\b/gu)).toHaveLength(1);
      expect(html).toContain('id="main"');
      expect(attributeValues(html, "pre", "tabindex").every((value) => value === "0")).toBe(true);
      expect(attributeValues(html, "pre > code", "class").every((value) => value.includes("syntax-code"))).toBe(true);
      expect(attributeValues(html, '.xcb-docs-table-wrap', "tabindex").every((value) => value === "0")).toBe(true);
      expect(attributeValues(html, '.skip-link', 'href')).toEqual(['#main']);
      expect(attributeValues(html, 'nav[aria-label="Documentation"] a[aria-current="page"]', "href"))
        .toEqual([`/docs/${topic.slug}`]);
      const aiLinks = attributeValues(html, 'nav[aria-label="Ask AI about this"] a', "href");
      expect(aiLinks.length).toBeGreaterThan(0);
      for (const href of aiLinks) {
        expect(decodeURIComponent(href)).toContain(`https://xcb.sh/docs/${topic.slug}`);
      }
    }
    await expect(renderTopic("missing-topic")).rejects.toThrow("NEXT_HTTP_ERROR_FALLBACK;404");
  });

  test("gives overview and topics their own canonical and social URLs", async () => {
    expect(overviewMetadata.alternates?.canonical).toBe("/docs");
    const descriptions = new Set<string>();
    for (const topic of docsTopics) {
      const metadata = await generateMetadata({ params: Promise.resolve({ slug: topic.slug }) });
      expect(metadata.alternates?.canonical).toBe(`/docs/${topic.slug}`);
      expect(metadata.openGraph?.url).toBe(`/docs/${topic.slug}`);
      expect(metadata.title).toBe(`${topic.title} · Excalibur (xcb) docs`);
      expect(metadata.description).toBe(topic.description);
      // Page descriptions are unique sentences of 110 to 160 characters.
      expect(topic.description.length).toBeGreaterThanOrEqual(110);
      expect(topic.description.length).toBeLessThanOrEqual(160);
      descriptions.add(topic.description);
    }
    expect(descriptions.size).toBe(docsTopics.length);
    await expect(generateMetadata({ params: Promise.resolve({ slug: "missing-topic" }) }))
      .rejects.toThrow("NEXT_HTTP_ERROR_FALLBACK;404");
  });

  test("keeps inbound anchors on the overview and states each provider status once", () => {
    const html = renderToStaticMarkup(<Docs />);
    expect(html.match(/<h1\b/gu)).toHaveLength(1);
    expect(html).toContain('id="readiness"');
    expect(html).toContain('id="standalone-package"');
    expect(html).toContain('href="/docs/route"');
    expect(html).toContain('href="/docs/sdk"');
    for (const status of Object.values(providerStatus)) expect(occurrences(html, rendered(status))).toBe(1);
    if (publishedRelease !== null) expect(html).toContain(`v${publishedRelease.version}`);
    expect(attributeValues(html, 'nav[aria-label="Documentation"] a[aria-current="page"]', "href"))
      .toEqual(["/docs"]);
  });

  test("walks a first task from a verified install to a routed result", async () => {
    const html = await renderTopic("getting-started");
    if (publishedRelease === null) {
      expect(html).toContain("No verified release is published yet");
    } else {
      expect(textOf(html)).toContain("curl -fsSL https://xcb.sh/install.sh | sh");
      expect(html).toContain(`v${publishedRelease.version}`);
    }
    expect(textOf(html)).toContain("rustup toolchain install 1.97.1 --profile minimal");
    expect(textOf(html)).toContain("./scripts/install-native.sh");
    expect(textOf(html)).toContain("xcb setup claude");
    expect(textOf(html)).toContain("git init -q");
    expect(textOf(html)).toContain("xcb models route --task");
    expect(textOf(html)).toContain("xcb tasks");
    expect(html).toContain(`Claude Code ${supportedBuilds.claudeMinimum} or later`);
    expect(html).not.toContain("--label");
    expect(html).not.toMatch(/(?:npm|bun) (?:install|add) -g @hraness\/xcb/u);
  });

  test("documents the thread, task controls, and direct sessions", async () => {
    const html = await renderTopic("projects-and-tasks");
    expect(textOf(html)).toContain("xcb --cwd /absolute/path/to/your/project conversations --new --json");
    for (const command of ["xcb tasks --json", "xcb attention --json", "xcb inbox --task <task-id> --json", "xcb tasks cancel <task-id> --revision <revision> --json", "xcb history <session-id> --direct --json"]) {
      expect(textOf(html)).toContain(command);
    }
    expect(textOf(html)).not.toMatch(/xcb (?:chat|resume)\b/u);
    // A limit stated in plain words.
    expect(html).toContain("reading a saved session does not continue a provider turn");
  });

  test("lists supported provider builds and connection commands", async () => {
    const provider = await renderTopic("providers");
    expect(attributeValues(provider, '.xcb-docs-table-wrap[role="region"]', "aria-labelledby"))
      .toEqual(["provider-status-caption"]);
    expect(provider).toContain(`Claude Code ${supportedBuilds.claudeMinimum} or later within version 2`);
    for (const build of supportedBuilds.codex) expect(provider).toContain(build);
    for (const status of Object.values(providerStatus)) expect(occurrences(provider, rendered(status))).toBe(1);
    expect(textOf(provider)).toContain("xcb setup codex");
    expect(textOf(provider)).toContain("xcb accounts add claude --plan Max");
    expect(textOf(provider)).toContain("xcb accounts login <account-id>");
    expect(textOf(provider)).toContain("xcb accounts refresh <account-id>");
    expect(textOf(provider)).toContain("xcb accounts import-codex --source");
    expect(textOf(provider)).toContain("xcb doctor --provider claude --qualify-sandbox");
    expect(textOf(provider)).toContain("profile xcb-bwrap /usr/bin/bwrap flags=(unconfined)");
  });

  test("lists only provider builds the runtime or the reviewed catalog supports", async () => {
    const [catalogSource, claude, codex] = await Promise.all([
      readFile(join(repository, "qualified-builds.json"), "utf8"),
      readFile(join(repository, "crates/xcb-runtime/src/claude.rs"), "utf8"),
      readFile(join(repository, "crates/xcb-runtime/src/codex/config.rs"), "utf8"),
    ]);
    const catalog = JSON.parse(catalogSource) as Record<string, unknown>;
    const catalogVersions = (provider: string): string[] =>
      ((catalog[provider] ?? []) as { version: string }[]).map((entry) => entry.version);
    expect(claude).toContain(`pub const MIN_VERSION: &str = "${supportedBuilds.claudeMinimum}";`);
    expect(claude).toContain("pub const MAX_MAJOR: u64 = 2;");
    for (const build of supportedBuilds.codex) {
      expect(catalogVersions("codex").includes(build) || codex.includes(`"${build}"`)).toBe(true);
    }
  });

  test("keeps the application route separate from coding sessions", async () => {
    const application = await renderTopic("application-api");
    expect(textOf(application)).toContain("xcb --json generate --capabilities");
    expect(application).toContain("without provider refresh or inference");
    expect(application).toContain("available: true");
    expect(application).toContain("supported: false");
    expect(application).toContain("All six fields are required");
    expect(application).toContain("1 MiB");
    expect(application).toContain("1,000 to 300,000 milliseconds");
    expect(application).toContain("1 to 262,144 bytes");
    expect(application).toContain("admission: &quot;pending&quot;");
    expect(application).toContain("xcb application disable");
    expect(application).toContain("close stdin");
    expect(application).toContain("90 seconds is a practical desktop integration recommendation");
    expect(application).toContain("not a protocol timing guarantee");
    expect(application).toContain("drain stdout and stderr");
    expect(application).toContain("HOME");
    expect(application).toContain("XCB_STATE");
    expect(application).toContain("never saves your prompts or replies");
  });

  test("documents pane controls, extensions, and the turn deadline", async () => {
    const html = await renderTopic("customization");
    expect(html).toContain("The interactive terminal is removed");
    expect(textOf(html)).toContain("xcb panes check /absolute/path/to/pane.json");
    expect(html).not.toContain("/pane generate");
    expect(textOf(html)).toContain("xcb plugins disable auto-continue");
    expect(html).toContain("turn_timeout_ms");
    expect(html).toContain("1,000 to 3,600,000 milliseconds");
    expect(textOf(html)).toContain("xcb judge clef --model clef");
    expect(textOf(html)).toContain("CLOUDFLARE_ACCOUNT_ID");
    expect(textOf(html)).toContain("CLOUDFLARE_API_TOKEN");
  });

  test("documents offline command-runner setup and its limits without internal tools", async () => {
    const html = await renderTopic("workspace");
    expect(html).toContain("scripts/setup-command-runner.py");
    expect(html).toContain("source checkout matching your installed native CLI");
    expect(html).toContain("Lima 2.2 or later");
    expect(html).toContain("--dry-run");
    expect(html).toContain("--prepare");
    expect(textOf(html)).toContain("--status --cache-key CACHE_KEY_FROM_PLAN");
    expect(html).toContain("10 minutes");
    expect(html).toContain("macOS ARM64 only");
    expect(html).toContain("Each file replacement is atomic; the entire batch is not a transaction");
    if (publishedRelease !== null) expect(textOf(html)).toContain(`--branch v${publishedRelease.version}`);
    expect(html).not.toContain("host-run");
  });

  test("gives recovery steps without deleting state", async () => {
    const html = await renderTopic("troubleshooting");
    expect(textOf(html)).toContain("xcb recover <run-id> --yes");
    expect(html).toContain("Do not delete lock files");
    expect(textOf(html)).toContain("xcb doctor --provider claude --executable /absolute/path/to/claude");
    expect(textOf(html)).toContain("xcb accounts login <account>");
    expect(html).toContain("x-apple.systempreferences:com.apple.preference.security?Privacy_FilesAndFolders");
    // xcb installs with curl; there is no browser download to unblock.
    expect(html).not.toContain("xattr -d com.apple.quarantine");
  });

  test("covers upgrades, uninstall, and every folder xcb creates", async () => {
    const html = await renderTopic("upgrade-and-uninstall");
    expect(textOf(html)).toContain("xcb update check");
    expect(textOf(html)).toContain("xcb upgrade <version>");
    expect(textOf(html)).toContain("xcb service uninstall");
    expect(textOf(html)).toContain("rm ~/.local/bin/xcb");
    for (const path of ["~/.local/bin/xcb", "~/.local/share/xcb", "~/.local/share/xcb-command", "~/.local/share/xcb-coordination", "dev.hraness.xcb.update.plist", "dev.hraness.xcb.habitat.*.plist", "~/.xcb"]) {
      expect(html).toContain(path);
    }
    // Versions come from the release datum, never typed by hand.
    expect(html).not.toMatch(/xcb upgrade \d/u);
  });

  test("describes the route request, results, and every failure code", async () => {
    const html = await renderTopic("route");
    expect(attributeValues(html, "pre > code", "data-language")).toContain("json");
    expect(html).toContain("xcb --json route");
    expect(html).toContain("dryRun");
    expect(html).toContain("256 KiB");
    expect(html).toContain("1,000 to 3,600,000");
    for (const code of ["invalid_request", "unavailable", "busy", "deadline", "cancelled", "provider_error", "needs_input", "custody_unproven"]) {
      expect(html).toContain(`<code>${code}</code>`);
    }
    // The homepage links /docs/route#sdk.
    expect(html).toContain('id="sdk"');
    expect(html).toContain('href="/docs/sdk"');
    // /compare/claude-code links /docs/route#call-from-an-agent; the agent section previews first.
    expect(html).toContain('id="call-from-an-agent"');
    expect(textOf(html)).toContain('"dryRun": true');
  });

  test("installs the SDK from npm and imports only exported names", async () => {
    const html = await renderTopic("sdk");
    expect(attributeValues(html, "pre > code", "data-language")).toEqual(["shell", "typescript", "text"]);
    expect(textOf(html)).toContain("npm install @hraness/xcb");
    expect(textOf(html)).toContain("bun add @hraness/xcb");
    expect(html).not.toMatch(/npm install -g @hraness\/xcb/u);
    expect(html).toContain("createSubscriptionRouter");
    expect(html).toContain('href="/docs/route"');
    // src/index.ts is the package's public surface: named exports plus `export *` modules.
    const index = await readFile(join(repository, "src/index.ts"), "utf8");
    const starSources = await Promise.all([...index.matchAll(/export \* from "\.\/([^"]+)"/gu)]
      .map((match) => readFile(join(repository, "src", match[1]!), "utf8")));
    const exported = (name: string): boolean =>
      new RegExp(`export (?:type )?\\{[^}]*\\b${name}\\b[^}]*\\}`, "u").test(index)
      || starSources.some((source) => new RegExp(`export (?:async )?(?:class|function|type|interface|const) ${name}\\b`, "u").test(source));
    const example = textOf(html).match(/import \{([^}]+)\} from "@hraness\/xcb"/u);
    expect(example).not.toBeNull();
    const names = example![1]!.split(",").map((name) => name.replace(/^\s*type\s+/u, "").trim()).filter(Boolean);
    expect(names).toContain("createSubscriptionRouter");
    for (const name of names) expect({ name, exported: exported(name) }).toEqual({ name, exported: true });
    // The repository's Markdown twin carries the same example and output.
    const markdown = await readFile(join(repository, "docs/sdk.md"), "utf8");
    expect(markdown).toContain(`\`\`\`ts\n${sdkExample}\n\`\`\``);
    expect(markdown).toContain(`\`\`\`text\n${sdkExampleOutput}\n\`\`\``);
  });

  test("lists every command, setting, environment variable, and exit code", async () => {
    const html = await renderTopic("reference");
    for (const command of ["setup", "run", "doctor", "accounts", "models", "offers", "conversations", "workspaces", "history", "rename", "tasks", "backlog", "steer", "watch", "inbox", "attention", "schedules", "sessions", "service", "update", "upgrade", "completions", "daemons", "projects", "memory", "reflex", "panes", "plugins", "hooks", "judge", "config", "recover", "command", "generate", "route"]) {
      expect(html).toMatch(new RegExp(`<code>xcb [^<]*\\b${command}\\b`, "u"));
    }
    for (const removed of ["chat", "resume", "link", "fleet", "dispatch", "send", "remote"]) {
      expect(textOf(html)).not.toMatch(new RegExp(`xcb ${removed}\\b`, "u"));
    }
    for (const key of ["turn_timeout_ms", "default_account", "favorites", "auto_failover", "extensions.auto_continue", "extensions.gobstopper", "extensions.judge", "extensions.reflexes"]) {
      expect(html).toContain(`<code>${key}</code>`);
    }
    for (const variable of ["XCB_STATE", "XCB_COORDINATION_ROOT", "XCB_JEV_API_KEY", "XCB_RELAY_URL", "NO_COLOR", "XCB_VERSION", "XCB_INSTALL_PREFIX", "XCB_ADD_PATH"]) {
      expect(html).toContain(`<code>${variable}</code>`);
    }
    for (const id of ["configuration", "environment", "files", "exit-codes", "json"]) expect(html).toContain(`id="${id}"`);
    expect(html).toContain("~/.local/share/xcb");
  });

  test("states what the optional judge sends", async () => {
    const html = await renderTopic("security");
    for (const fact of ["api.cloudflare.com", "128 KiB", "8 KiB", "88 KiB", "port 443", "~/.local/share/xcb"]) expect(html).toContain(fact);
    expect(html).toContain('id="judge"');
  });

  test("uses a responsive navigation and no developer-specific account or home paths", async () => {
    const [css, content] = await Promise.all([
      readFile(join(import.meta.dir, "../app/docs/docs.css"), "utf8"),
      readFile(join(import.meta.dir, "../app/docs/topic-content.tsx"), "utf8"),
    ]);
    expect(css).toContain("@media (max-width: 48rem)");
    expect(css).toContain(".xcb-docs-sidebar { position: static;");
    expect(css).toContain('a[aria-current="page"]');
    expect(css).toContain(".xcb-docs-body pre:focus-visible");
    expect(css).toContain(".xcb-docs-table-wrap:focus-visible");
    expect(content).not.toMatch(/\/Users\/[a-z]|a_[a-f0-9]{32}|98c6cc850848/u);
    // Authored copy uses no em dashes and no version-stamped history.
    expect(content).not.toContain("—");
    expect(content).not.toMatch(/\bv0\.\d+(?:\.\d+)?\b/u);
  });
});

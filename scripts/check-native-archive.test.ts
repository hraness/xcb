import { afterEach, expect, test } from "bun:test";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdtempSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { gzipSync } from "node:zlib";

const roots: string[] = [];
afterEach(() => { for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true }); });
const hash = (bytes: Uint8Array) => createHash("sha256").update(bytes).digest("hex");
const binary = (version: string) => `#!/bin/sh\n[ "$#" = 1 ] && [ "$1" = --version ] || exit 17\nprintf 'xcb ${version}\\n'\n`;

type Entry = { name: string; type?: string; contents?: string; link?: string };
function archive(entries: readonly Entry[]): Buffer {
  const blocks: Buffer[] = [];
  for (const entry of entries) {
    const contents = Buffer.from(entry.contents ?? "");
    const header = Buffer.alloc(512);
    const text = (offset: number, length: number, value: string) => header.write(value, offset, length, "utf8");
    const octal = (offset: number, length: number, value: number) => text(offset, length, value.toString(8).padStart(length - 1, "0") + "\0");
    text(0, 100, entry.name);
    octal(100, 8, 0o755); octal(108, 8, 0); octal(116, 8, 0);
    octal(124, 12, contents.length); octal(136, 12, 0);
    header.fill(32, 148, 156);
    text(156, 1, entry.type ?? "0"); text(157, 100, entry.link ?? "");
    text(257, 6, "ustar\0"); text(263, 2, "00");
    text(148, 8, header.reduce((sum, byte) => sum + byte, 0).toString(8).padStart(6, "0") + "\0 ");
    blocks.push(header, contents, Buffer.alloc((512 - contents.length % 512) % 512));
  }
  return gzipSync(Buffer.concat([...blocks, Buffer.alloc(1024)]));
}

function fixture(entries: readonly Entry[], options: { checksum?: string; name?: string } = {}) {
  const root = realpathSync(mkdtempSync(join(tmpdir(), "xcb-native-archive-")));
  roots.push(root);
  const bytes = archive(entries);
  const path = join(root, options.name ?? "xcb-0.4.0-linux-x86_64.tar.gz");
  writeFileSync(path, bytes);
  writeFileSync(`${path}.sha256`, `${options.checksum ?? hash(bytes)}\n`);
  return path;
}

function check(version: string, ...archives: string[]) {
  return spawnSync("/bin/sh", [new URL("./check-native-archive.sh", import.meta.url).pathname, version, ...archives], {
    encoding: "utf8", timeout: 10_000, maxBuffer: 64 * 1024, env: { PATH: "/usr/bin:/bin", LC_ALL: "C" },
  });
}

test("one regular xcb member with a matching checksum and version is admitted", () => {
  const path = fixture([{ name: "xcb", contents: binary("0.4.0") }]);
  const result = check("v0.4.0", path);
  expect(result.status).toBe(0);
  expect(result.stdout).toContain("ok: xcb-0.4.0-linux-x86_64.tar.gz");
  expect(result.stdout).toContain("reports 'xcb 0.4.0'");
});

test("an AppleDouble companion or any second member is rejected", () => {
  const path = fixture([{ name: "._xcb", contents: "metadata" }, { name: "xcb", contents: binary("0.4.0") }]);
  const result = check("0.4.0", path);
  expect(result.status).toBe(1);
  expect(result.stderr).toContain("must list exactly one member 'xcb'");
});

test("a non-regular xcb entry is rejected", () => {
  const path = fixture([{ name: "xcb", type: "2", link: "/bin/sh" }]);
  const result = check("0.4.0", path);
  expect(result.status).toBe(1);
  expect(result.stderr).toContain("regular file");
});

test("a checksum that does not describe the archive bytes is rejected", () => {
  const path = fixture([{ name: "xcb", contents: binary("0.4.0") }], { checksum: "0".repeat(64) });
  const result = check("0.4.0", path);
  expect(result.status).toBe(1);
  expect(result.stderr).toContain("checksum mismatch");
});

test("a binary reporting another version or a foreign archive name is rejected", () => {
  const stale = fixture([{ name: "xcb", contents: binary("0.3.0") }]);
  expect(check("0.4.0", stale).stderr).toContain("expected 'xcb 0.4.0'");
  const foreign = fixture([{ name: "xcb", contents: binary("0.4.0") }], { name: "xcb-0.4.0-linux-x86_64.zip" });
  expect(check("0.4.0", foreign).stderr).toContain("is not an xcb-0.4.0-<os>-<arch>.tar.gz release archive");
  expect(check("latest", stale).stderr).toContain("stable semantic version");
});

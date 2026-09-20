import { afterEach, expect, test } from "bun:test";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  closeSync, copyFileSync, existsSync, mkdirSync, mkdtempSync, openSync,
  readFileSync, readdirSync, realpathSync, rmSync, statSync, symlinkSync, unlinkSync, writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { gzipSync } from "node:zlib";

const roots: string[] = [];
afterEach(() => { for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true }); });
const hash = (bytes: Uint8Array | string) => createHash("sha256").update(bytes).digest("hex");
const binary = (version: string, label = "candidate") => `#!/bin/sh\n[ "$#" = 1 ] && [ "$1" = --version ] || exit 17\nprintf 'xcb ${version}\\n'\n# ${label}\n`;

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

function fixture(version = "0.4.0") {
  const root = realpathSync(mkdtempSync(join(tmpdir(), "xcb-installer-")));
  roots.push(root);
  const repository = join(root, "repo"), prefix = join(root, "prefix"), stubs = join(root, "stubs");
  for (const path of [join(repository, "scripts"), join(repository, "target/release"), join(prefix, "bin"), stubs, join(root, "home")]) mkdirSync(path, { recursive: true });
  copyFileSync(new URL("./install-native.sh", import.meta.url), join(repository, "scripts/install-native.sh"));
  writeFileSync(join(repository, "Cargo.toml"), '[workspace.package]\nversion = "0.4.0"\n');
  const candidate = join(root, "candidate"), destination = join(prefix, "bin/xcb");
  const previous = binary("0.4.0", "preserved previous build");
  writeFileSync(candidate, binary(version), { mode: 0o755 });
  writeFileSync(destination, previous, { mode: 0o755 });
  writeFileSync(join(stubs, "cargo"), `#!/bin/sh
set -eu
printf '%s\\n' "$@" > "$FIXTURE_CARGO_LOG"
install_root=
if [ "$1" = install ]; then
  [ "$#" = 9 ] && [ "$2" = --path ] && [ "$3" = "$PWD/crates/xcb-cli" ] && [ "$4" = --locked ] && [ "$5" = --bin ] && [ "$6" = xcb ] && [ "$7" = --root ] && [ "$9" = --no-track ] || exit 18
  install_root=$8
  case "$install_root" in "$XCB_INSTALL_PREFIX"/bin/.xcb-install.*/cargo-install) ;; *) exit 18 ;; esac
else
  [ "$*" = 'build --release --locked -p xcb-cli' ] || exit 18
fi
config_value() {
  [ -f .cargo/config.toml ] || return 0
  awk -F '"' -v key="$1" '$1 ~ "^" key "[[:space:]]*=" { print $2; exit }' .cargo/config.toml
}
target_dir=\${CARGO_TARGET_DIR:-$(config_value target-dir)}
target_dir=\${target_dir:-target}
target_triple=\${CARGO_BUILD_TARGET:-$(config_value target)}
artifact_dir="$target_dir/release"
if [ -n "$target_triple" ]; then artifact_dir="$target_dir/$target_triple/release"; fi
mkdir -p "$artifact_dir"
cp "$FIXTURE_BINARY" "$artifact_dir/xcb"
if [ -n "$install_root" ] && [ "\${FIXTURE_CARGO_SKIP_INSTALL:-}" != yes ]; then
  mkdir -p "$install_root/bin"
  cp "$artifact_dir/xcb" "$install_root/bin/xcb"
fi
`, { mode: 0o755 });
  writeFileSync(join(stubs, "curl"), `#!/bin/sh\n[ "$#" = 4 ] && [ "$1" = -fsSL ] && [ "$2" = -o ] || exit 19\ncase "$4" in\n *.tar.gz.sha256) cp "$FIXTURE_CHECKSUM" "$3" ;;\n *.tar.gz) cp "$FIXTURE_ARCHIVE" "$3" ;;\n *) exit 20 ;;\nesac\n`, { mode: 0o755 });
  writeFileSync(join(stubs, "tar"), `#!/bin/sh\nif [ "$1" = -xzOf ]; then printf 'extract\\n' > "$FIXTURE_EXTRACT_LOG"; fi\nexec /usr/bin/tar "$@"\n`, { mode: 0o755 });
  const archivePath = join(root, "archive.tar.gz"), checksum = join(root, "checksum");
  function release(entries: readonly Entry[] = [{ name: "xcb", contents: binary(version) }], corruptChecksum = false) {
    const bytes = archive(entries);
    writeFileSync(archivePath, bytes);
    writeFileSync(checksum, (corruptChecksum ? "0".repeat(64) : hash(bytes)) + "\n");
  }
  function run(fromRelease = false, extra: Record<string, string> = {}) {
    return spawnSync("/bin/sh", [join(repository, "scripts/install-native.sh")], {
      cwd: repository, encoding: "utf8", timeout: 10_000, maxBuffer: 64 * 1024,
      env: {
        PATH: `${stubs}:/usr/bin:/bin`, HOME: join(root, "home"), LC_ALL: "C",
        CARGO: join(stubs, "cargo"), XCB_INSTALL_PREFIX: prefix, XCB_VERSION: fromRelease ? "v0.4.0" : "", XCB_ADD_PATH: "ask",
        FIXTURE_BINARY: candidate, FIXTURE_CARGO_LOG: join(root, "cargo.log"),
        FIXTURE_ARCHIVE: archivePath, FIXTURE_CHECKSUM: checksum, FIXTURE_EXTRACT_LOG: join(root, "extract.log"), ...extra,
      },
    });
  }
  function unchanged() {
    expect(readFileSync(destination, "utf8")).toBe(previous);
    expect(readdirSync(join(prefix, "bin"))).toEqual(["xcb"]);
  }
  return { root, repository, prefix, candidate, destination, previous, release, run, unchanged };
}

test("native source upgrade validates staged bytes, atomically replaces, and backs up the old digest", () => {
  const f = fixture();
  const old = openSync(f.destination, "r"), inode = statSync(f.destination).ino;
  try {
    const result = f.run();
    expect(result.status).toBe(0);
    expect(readFileSync(f.destination, "utf8")).toBe(binary("0.4.0"));
    expect(readFileSync(old, "utf8")).toBe(f.previous);
    expect(statSync(f.destination).ino).not.toBe(inode);
    const backup = join(f.prefix, `bin/xcb.previous.${hash(f.previous)}`);
    expect(readFileSync(backup, "utf8")).toBe(f.previous);
    expect(statSync(backup).nlink).toBe(1);
    expect(statSync(backup).mode & 0o777).toBe(0o500);
    expect(result.stdout).toContain(`Previous binary preserved at ${backup}`);
    expect(result.stdout).toContain("Restart open xcb terminals, then run xcb doctor");
    const cargoArgs = readFileSync(join(f.root, "cargo.log"), "utf8").trim().split("\n");
    expect(cargoArgs.slice(0, 7)).toEqual(["install", "--path", join(f.repository, "crates/xcb-cli"), "--locked", "--bin", "xcb", "--root"]);
    expect(cargoArgs[7]).toStartWith(join(f.prefix, "bin/.xcb-install."));
    expect(cargoArgs[7]).toEndWith("/cargo-install");
    expect(cargoArgs[8]).toBe("--no-track");
    expect(readdirSync(join(f.prefix, "bin")).some(name => name.startsWith(".xcb-install"))).toBe(false);
  } finally { closeSync(old); }
});

for (const configured of ["environment", "cargo-config"] as const) {
  test(`native source installs Cargo's actual artifact with ${configured} overrides and a same-version stale default`, () => {
    const f = fixture();
    const stale = binary("0.4.0", "stale default artifact must not be installed");
    const defaultArtifact = join(f.repository, "target/release/xcb");
    writeFileSync(defaultArtifact, stale, { mode: 0o755 });
    const alternate = join(f.root, "alternate target directory");
    let extra: Record<string, string> = {};
    if (configured === "environment") {
      extra = { CARGO_TARGET_DIR: alternate, CARGO_BUILD_TARGET: "fixture-target" };
    } else {
      mkdirSync(join(f.repository, ".cargo"));
      writeFileSync(join(f.repository, ".cargo/config.toml"), `[build]\ntarget-dir = "${alternate}"\ntarget = "fixture-target"\n`);
    }
    const result = f.run(false, extra);
    expect(result.status).toBe(0);
    expect(readFileSync(defaultArtifact, "utf8")).toBe(stale);
    expect(readFileSync(join(alternate, "fixture-target/release/xcb"), "utf8")).toBe(binary("0.4.0"));
    expect(readFileSync(f.destination, "utf8")).toBe(binary("0.4.0"));
    expect(readFileSync(join(f.prefix, `bin/xcb.previous.${hash(f.previous)}`), "utf8")).toBe(f.previous);
  });
}

test("native source never falls back to a stale default when Cargo does not stage its executable", () => {
  const f = fixture();
  const defaultArtifact = join(f.repository, "target/release/xcb");
  const stale = binary("0.4.0", "stale default artifact must not be installed");
  writeFileSync(defaultArtifact, stale, { mode: 0o755 });
  const result = f.run(false, { CARGO_TARGET_DIR: join(f.root, "alternate"), FIXTURE_CARGO_SKIP_INSTALL: "yes" });
  expect(result.status).not.toBe(0);
  expect(result.stderr).toContain("source candidate must be a regular non-symlink file");
  expect(readFileSync(defaultArtifact, "utf8")).toBe(stale);
  f.unchanged();
});

test("native source version mismatch preserves the installed binary", () => {
  const f = fixture("0.4.1");
  const result = f.run();
  expect(result.status).not.toBe(0);
  expect(result.stderr).toContain("expected 'xcb 0.4.0'");
  f.unchanged();
});

test("native candidate execution failure preserves the installed binary", () => {
  const f = fixture();
  writeFileSync(f.candidate, "#!/bin/sh\nexit 9\n", { mode: 0o755 });
  expect(f.run().status).not.toBe(0);
  f.unchanged();
});

test("native release installs only the verified regular binary without invoking Cargo", () => {
  const f = fixture();
  unlinkSync(f.destination);
  f.release();
  const result = f.run(true);
  expect(result.status).toBe(0);
  expect(readFileSync(f.destination, "utf8")).toBe(binary("0.4.0"));
  expect(statSync(f.destination).mode & 0o777).toBe(0o755);
  expect(existsSync(join(f.root, "cargo.log"))).toBe(false);
  expect(readdirSync(join(f.prefix, "bin"))).toEqual(["xcb"]);
});

test("native invalid release coordinate never falls back to a source build", () => {
  const f = fixture();
  expect(f.run(true, { XCB_VERSION: "v" }).status).not.toBe(0);
  expect(existsSync(join(f.root, "cargo.log"))).toBe(false);
  f.unchanged();
});

test("native release checksum mismatch preserves the installed binary", () => {
  const f = fixture();
  f.release(undefined, true);
  const result = f.run(true);
  expect(result.status).not.toBe(0);
  expect(result.stderr).toContain("checksum mismatch");
  f.unchanged();
});

test("native release binary version mismatch preserves the installed binary", () => {
  const f = fixture("0.4.1");
  f.release();
  expect(f.run(true).status).not.toBe(0);
  f.unchanged();
});

for (const [label, entries] of Object.entries({
  traversal: [{ name: "../escaped", contents: binary("0.4.0") }],
  absolute: [{ name: "/xcb", contents: binary("0.4.0") }],
  symlink: [{ name: "xcb", type: "2", link: "../escaped" }],
  hardlink: [{ name: "xcb", type: "1", link: "../escaped" }],
  directory: [{ name: "xcb", type: "5" }],
  extra: [{ name: "xcb", contents: binary("0.4.0") }, { name: "extra", contents: "forbidden" }],
  duplicate: [{ name: "xcb", contents: binary("0.4.0") }, { name: "xcb", contents: binary("0.4.0") }],
} satisfies Record<string, Entry[]>)) {
  test(`native release refuses ${label} archive entries before extraction`, () => {
    const f = fixture();
    f.release(entries);
    const result = f.run(true);
    expect(result.status).not.toBe(0);
    f.unchanged();
    expect(existsSync(join(f.root, "extract.log"))).toBe(false);
    expect(existsSync(join(f.prefix, "bin/escaped"))).toBe(false);
    expect(existsSync(join(f.root, "escaped"))).toBe(false);
  });
}

test("native upgrade refuses an unsafe digest backup without changing either file", () => {
  const f = fixture();
  const target = join(f.root, "unrelated");
  writeFileSync(target, "preserve unrelated data");
  symlinkSync(target, join(f.prefix, `bin/xcb.previous.${hash(f.previous)}`));
  expect(f.run().status).not.toBe(0);
  expect(readFileSync(f.destination, "utf8")).toBe(f.previous);
  expect(readFileSync(target, "utf8")).toBe("preserve unrelated data");
});

test("native upgrade reuses only an unchanged digest backup", () => {
  const f = fixture();
  const backup = join(f.prefix, `bin/xcb.previous.${hash(f.previous)}`);
  writeFileSync(backup, f.previous, { mode: 0o755 });
  expect(f.run().status).toBe(0);
  expect(readFileSync(backup, "utf8")).toBe(f.previous);
});

test("native upgrade refuses a symlink destination and leaves its target untouched", () => {
  const f = fixture();
  const target = join(f.root, "unrelated");
  writeFileSync(target, "preserve unrelated data");
  unlinkSync(f.destination);
  symlinkSync(target, f.destination);
  expect(f.run().status).not.toBe(0);
  expect(readFileSync(target, "utf8")).toBe("preserve unrelated data");
});

test("native install does not bypass an existing installation owner", () => {
  const f = fixture();
  mkdirSync(join(f.prefix, "bin/.xcb-install-lock"));
  expect(f.run().status).not.toBe(0);
  expect(readFileSync(f.destination, "utf8")).toBe(f.previous);
  expect(existsSync(join(f.root, "cargo.log"))).toBe(false);
  expect(existsSync(join(f.prefix, "bin/.xcb-install-lock"))).toBe(true);
});

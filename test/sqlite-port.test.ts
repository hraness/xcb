import { describe, expect, test } from "bun:test";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { wrapSqliteDatabase } from "../src/sqlite-port.ts";

describe("SQLite statement lifecycle", () => {
  test("close finalizes retained statements before the database and rejects later use", () => {
    const events: string[] = [];
    const database = wrapSqliteDatabase({
      exec() {},
      prepare(sql) {
        return {
          get: () => ({ value: 1 }), all: () => [{ value: 1 }], run: () => ({ changes: 1 }),
          finalize: () => { events.push(sql); },
        };
      },
      close: () => { events.push("close"); },
    });
    const first = database.query("first"), second = database.query("second");
    expect(first.get()).toEqual({ value: 1 });
    expect(second.all()).toEqual([{ value: 1 }]);
    database.close();
    database.close();
    expect(events).toEqual(["first", "second", "close"]);
    for (const use of [() => first.get(), () => first.all(), () => first.run(), () => second.get(),
      () => database.exec("select 1"), () => database.query("select 1")]) {
      expect(use).toThrow("SQLITE_DATABASE_CLOSED");
    }
  });

  test("statement finalization can be retried before closing the database", () => {
    let attempts = 0, closes = 0;
    const database = wrapSqliteDatabase({
      exec() {},
      prepare: () => ({
        get: () => null, all: () => [], run: () => ({ changes: 0 }),
        finalize() { if (++attempts === 1) throw new Error("finalization failed"); },
      }),
      close() { closes++; },
    });
    const retained = database.query("select 1");
    expect(() => database.close()).toThrow("finalization failed");
    expect(closes).toBe(0);
    database.close();
    expect(attempts).toBe(2);
    expect(closes).toBe(1);
    expect(() => retained.get()).toThrow("SQLITE_DATABASE_CLOSED");
  });

  test("a failed native close remains retryable without finalizing statements twice", () => {
    let finalizations = 0, closes = 0;
    const database = wrapSqliteDatabase({
      exec() {},
      prepare: () => ({
        get: () => null, all: () => [], run: () => ({ changes: 0 }),
        finalize() { finalizations++; },
      }),
      close() { if (++closes === 1) throw new Error("close failed"); },
    });
    const retained = database.query("select 1");
    expect(() => database.close()).toThrow("close failed");
    database.close();
    database.close();
    expect(finalizations).toBe(1);
    expect(closes).toBe(2);
    expect(() => retained.get()).toThrow("SQLITE_DATABASE_CLOSED");
  });

  for (const mode of ["delete", "wal", "read-error"]) {
    test(`DELETE initialization finalizes its journal query (${mode})`, () => {
      const events: string[] = [];
      const open = () => wrapSqliteDatabase({
        exec(sql) { events.push(sql); },
        prepare: () => ({
          get() { if (mode === "read-error") throw new Error("read failed"); return { journal_mode: mode }; },
          all: () => [], run: () => ({ changes: 0 }),
          finalize() { events.push("finalize"); },
        }),
        close() { events.push("close"); },
      }, "DELETE");
      if (mode === "delete") open().close();
      else expect(open).toThrow(mode === "wal" ? "SQLITE_JOURNAL_MISMATCH" : "read failed");
      expect(events).toEqual(mode === "delete"
        ? ["PRAGMA busy_timeout=0", "finalize", "PRAGMA foreign_keys=OFF", "close"]
        : ["PRAGMA busy_timeout=0", "finalize"]);
    });
  }
});

// Keep the closing process alive with references to its prepared statements while
// another process immediately acquires the database. No GC or timeout can release
// the transaction on its behalf. Run the same contract against both native ports.
for (const runtime of ["bun", "node"] as const) {
  for (const journal of ["WAL", "DELETE"] as const) {
    test.skipIf(runtime === "node" && Bun.which("node") === null)(`${runtime} ${journal}: close releases locks with retained, reusable statements`, async () => {
      const root = await mkdtemp(join(tmpdir(), "xcb-sqlite-close-"));
      try {
        const moduleUrl = new URL("../src/sqlite-port.ts", import.meta.url).href;
        const databasePath = join(root, "database.sqlite");
        const argumentsPrefix = runtime === "node" ? ["--no-warnings", "--experimental-strip-types", "--input-type=module"] : [];
        const writer = `import assert from "node:assert/strict";
          import { openAccountDatabase } from ${JSON.stringify(moduleUrl)};
          const database = await openAccountDatabase(${JSON.stringify(databasePath)}, ${JSON.stringify(journal)});
          database.exec("PRAGMA busy_timeout=0; BEGIN IMMEDIATE");
          assert.equal(database.query("SELECT count(*) AS count FROM entries").get().count, 2);
          database.exec("INSERT INTO entries(value) VALUES(2); COMMIT");
          database.close();
          console.log("writer acquired and committed");`;
        const scenario = `import assert from "node:assert/strict";
          import { spawnSync } from "node:child_process";
          import { openAccountDatabase } from ${JSON.stringify(moduleUrl)};
          const database = await openAccountDatabase(${JSON.stringify(databasePath)}, ${JSON.stringify(journal)});
          if (${JSON.stringify(journal)} === "DELETE") assert.equal(database.query("PRAGMA busy_timeout").get().timeout, 0);
          database.exec("CREATE TABLE entries (id INTEGER PRIMARY KEY, value INTEGER NOT NULL)");
          const insert = database.query("INSERT INTO entries(value) VALUES(?)");
          assert.equal(insert.run(true).changes, 1);
          assert.equal(insert.run(false).changes, 1);
          const select = database.query("SELECT value FROM entries WHERE value = ?");
          assert.equal(select.get(true).value, 1);
          assert.equal(select.get(false).value, 0);
          assert.equal(select.get(5), null);
          assert.equal(select.all(true)[0].value, 1);
          assert.equal(select.all(false)[0].value, 0);
          database.exec("BEGIN IMMEDIATE; INSERT INTO entries(value) VALUES(99)");
          const retained = database.query("SELECT value FROM entries ORDER BY id");
          assert.equal(retained.get().value, 1);
          database.close();
          database.close();
          const child = spawnSync(process.execPath, [...${JSON.stringify(argumentsPrefix)}, "--eval", ${JSON.stringify(writer)}],
            { encoding: "utf8", timeout: 10000, maxBuffer: 65536 });
          assert.equal(child.error, undefined);
          assert.equal(child.status, 0, child.stderr);
          assert.equal(child.stdout.trim(), "writer acquired and committed");
          for (const use of [() => retained.get(), () => select.all(true), () => insert.run(true),
            () => database.exec("SELECT 1"), () => database.query("SELECT 1")]) assert.throws(use, /SQLITE_DATABASE_CLOSED/);
          console.log("reusable bindings, synchronous lock release, rollback, and closed handles verified");`;
        const child = Bun.spawn([runtime === "bun" ? process.execPath : Bun.which("node")!, ...argumentsPrefix, "--eval", scenario], {
          stdout: "pipe", stderr: "pipe", timeout: 15000,
        });
        const [code, stdout, stderr] = await Promise.all([child.exited, new Response(child.stdout).text(), new Response(child.stderr).text()]);
        expect(code, stderr).toBe(0);
        expect(stdout).toContain("reusable bindings, synchronous lock release, rollback, and closed handles verified");
      } finally { await rm(root, { recursive: true, force: true }); }
    });
  }
}

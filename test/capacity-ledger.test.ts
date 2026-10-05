import { describe, expect, test } from "bun:test";
import { Database } from "bun:sqlite";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { SqliteCapacityLedger, type CapacityLimits } from "../src/capacity-ledger.ts";

const limits: CapacityLimits = { maxRuns: 2, memoryBytes: 100, diskBytes: 200, maxRunsPerAccount: 1 };
const request = (overrides: Partial<Parameters<SqliteCapacityLedger["reserve"]>[0]> = {}) => ({
  reservationId: "reservation-one", owner: "task-one", workspace: "workspace-one", class: "build" as const,
  provider: "codex" as const, accountId: "account-one", memoryBytes: 30, diskBytes: 40, createdAt: 100, ...overrides,
});

describe("durable capacity ledger", () => {
  test("reserves atomically and retries by stable reservation identity", () => {
    const db = new Database(":memory:"), ledger = new SqliteCapacityLedger(db, limits);
    try {
      const first = ledger.reserve(request());
      expect(ledger.reserve(request())).toEqual(first);
      expect(ledger.projection()).toMatchObject({ activeRuns: 1, usedMemoryBytes: 30, usedDiskBytes: 40, availableRuns: 1 });
      expect(() => ledger.reserve(request({ owner: "other-task" }))).toThrow("CAPACITY_RESERVATION_ID_CONFLICT");
    } finally { db.close(); }
  });

  test("enforces global, memory, disk and per-account capacity", () => {
    const db = new Database(":memory:"), ledger = new SqliteCapacityLedger(db, limits);
    try {
      ledger.reserve(request());
      expect(() => ledger.reserve(request({ reservationId: "account-two", owner: "task-two", accountId: "account-one" }))).toThrow("CAPACITY_ACCOUNT_EXHAUSTED");
      ledger.reserve(request({ reservationId: "task-two", owner: "task-two", accountId: "account-two", memoryBytes: 70, diskBytes: 160 }));
      expect(() => ledger.reserve(request({ reservationId: "task-three", owner: "task-three", accountId: "account-three" }))).toThrow("CAPACITY_RUNS_EXHAUSTED");
    } finally { db.close(); }
  });

  test("uncertain effects retain capacity until evidence-bearing settlement", () => {
    const db = new Database(":memory:"), ledger = new SqliteCapacityLedger(db, limits);
    try {
      const reservation = ledger.reserve(request());
      const uncertain = ledger.settle(reservation, "uncertain", 200);
      expect(uncertain.state).toBe("uncertain");
      expect(ledger.projection()).toMatchObject({ activeRuns: 1, uncertainRuns: 1 });
      expect(() => ledger.release(uncertain)).toThrow("CAPACITY_UNCERTAIN_REQUIRES_SETTLEMENT");
      expect(() => ledger.settle(uncertain, "settled", 300)).toThrow("CAPACITY_SETTLEMENT_EVIDENCE_REQUIRED");
      const settled = ledger.settle(uncertain, "settled", 300, "evidence-digest");
      expect(settled).toMatchObject({ state: "settled", evidenceDigest: "evidence-digest", settledAt: 300 });
      expect(ledger.projection().activeRuns).toBe(0);
    } finally { db.close(); }
  });

  test("rejects a reservation object with altered capacity identity", () => {
    const db = new Database(":memory:");
    try {
      const ledger = new SqliteCapacityLedger(db, limits);
      const reservation = ledger.reserve(request({ reservationId: "reservation-tamper", owner: "task-tamper" }));
      expect(() => ledger.release({ ...reservation, memoryBytes: reservation.memoryBytes + 1 })).toThrow("CAPACITY_RESERVATION_MISMATCH");
      expect(() => ledger.settle({ ...reservation, workspace: "other-workspace" }, "uncertain", 200)).toThrow("CAPACITY_RESERVATION_MISMATCH");
    } finally { db.close(); }
  });

  test("reservation and uncertainty survive database restart", () => {
    const root = mkdtempSync(join(tmpdir(), "xcb-capacity-")), path = join(root, "capacity.sqlite");
    try {
      const firstDb = new Database(path), firstLedger = new SqliteCapacityLedger(firstDb, limits);
      const reservation = firstLedger.reserve(request());
      firstLedger.settle(reservation, "uncertain", 200);
      firstDb.close();
      const secondDb = new Database(path), secondLedger = new SqliteCapacityLedger(secondDb, limits);
      expect(secondLedger.inspect("reservation-one")).toMatchObject({ state: "uncertain", owner: "task-one" });
      expect(secondLedger.projection()).toMatchObject({ activeRuns: 1, uncertainRuns: 1 });
      secondDb.close();
    } finally { rmSync(root, { recursive: true, force: true }); }
  });
});

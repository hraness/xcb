import type { SqliteDatabase } from "./sqlite-port.ts";
import { boundedText, identifier, safeInteger, type AgentProvider, provider } from "./validation.ts";

/**
 * Durable admission accounting for habitat programs.
 *
 * This ledger deliberately sits below a scheduler: it records reservations and
 * proves that a task has a bounded share of capacity before a provider is
 * started. An uncertain effect keeps its reservation until an explicit,
 * evidence-bearing settlement. There is no time based reclamation.
 */
export type CapacityClass = "foreground" | "build" | "maintenance" | "recovery";
export type CapacityState = "active" | "uncertain" | "settled" | "released";

export type CapacityLimits = Readonly<{
  maxRuns: number;
  memoryBytes: number;
  diskBytes: number;
  maxRunsPerAccount: number;
}>;

export type CapacityRequest = Readonly<{
  reservationId: string;
  owner: string;
  workspace: string;
  class: CapacityClass;
  provider?: AgentProvider;
  accountId?: string;
  memoryBytes: number;
  diskBytes: number;
  createdAt: number;
}>;

export type CapacityReservation = Readonly<CapacityRequest & {
  state: CapacityState;
  settledAt: number | null;
  evidenceDigest: string | null;
}>;

export type CapacityProjection = Readonly<{
  limits: CapacityLimits;
  activeRuns: number;
  uncertainRuns: number;
  usedMemoryBytes: number;
  usedDiskBytes: number;
  availableRuns: number;
  availableMemoryBytes: number;
  availableDiskBytes: number;
  accountRuns: Readonly<Record<string, number>>;
}>;

type Row = {
  reservation_id: string;
  owner: string;
  workspace: string;
  class: CapacityClass;
  provider: AgentProvider | null;
  account_id: string | null;
  memory_bytes: number;
  disk_bytes: number;
  created_at: number;
  state: CapacityState;
  settled_at: number | null;
  evidence_digest: string | null;
};

const TABLE = "xcb_capacity_reservations";
const ACTIVE_STATES = "('active','uncertain')";

export class SqliteCapacityLedger {
  constructor(private readonly db: SqliteDatabase, readonly limits: CapacityLimits) {
    validateLimits(limits);
    db.exec(`CREATE TABLE IF NOT EXISTS ${TABLE} (
      reservation_id TEXT PRIMARY KEY,
      owner TEXT NOT NULL,
      workspace TEXT NOT NULL,
      class TEXT NOT NULL,
      provider TEXT,
      account_id TEXT,
      memory_bytes INTEGER NOT NULL,
      disk_bytes INTEGER NOT NULL,
      created_at INTEGER NOT NULL,
      state TEXT NOT NULL,
      settled_at INTEGER,
      evidence_digest TEXT
    )`);
    db.exec(`CREATE INDEX IF NOT EXISTS xcb_capacity_active_idx ON ${TABLE}(state)`);
    db.exec(`CREATE INDEX IF NOT EXISTS xcb_capacity_account_idx ON ${TABLE}(provider, account_id, state)`);
  }

  /** Atomically reserve capacity, or return the same reservation on a retry. */
  reserve(request: CapacityRequest): CapacityReservation {
    const input = normalizeRequest(request);
    this.db.exec("BEGIN IMMEDIATE");
    try {
      const existing = this.row(input.reservationId);
      if (existing !== null) {
        assertSameRequest(existing, input);
        this.db.exec("COMMIT");
        return fromRow(existing);
      }
      const projection = this.projection();
      if (projection.activeRuns + 1 > this.limits.maxRuns) throw new Error("CAPACITY_RUNS_EXHAUSTED");
      if (projection.usedMemoryBytes + input.memoryBytes > this.limits.memoryBytes) throw new Error("CAPACITY_MEMORY_EXHAUSTED");
      if (projection.usedDiskBytes + input.diskBytes > this.limits.diskBytes) throw new Error("CAPACITY_DISK_EXHAUSTED");
      if (input.provider !== undefined && input.accountId !== undefined) {
        const key = accountKey(input.provider, input.accountId);
        if ((projection.accountRuns[key] ?? 0) + 1 > this.limits.maxRunsPerAccount) throw new Error("CAPACITY_ACCOUNT_EXHAUSTED");
      }
      this.db.query(`INSERT INTO ${TABLE}
        (reservation_id, owner, workspace, class, provider, account_id, memory_bytes, disk_bytes, created_at, state, settled_at, evidence_digest)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 'active', NULL, NULL)`).run(
        input.reservationId, input.owner, input.workspace, input.class, input.provider ?? null, input.accountId ?? null,
        input.memoryBytes, input.diskBytes, input.createdAt);
      const row = this.row(input.reservationId);
      if (row === null) throw new Error("CAPACITY_RESERVATION_LOST");
      this.db.exec("COMMIT");
      return fromRow(row);
    } catch (error) {
      try { this.db.exec("ROLLBACK"); } catch { /* preserve the admission error */ }
      throw error;
    }
  }

  /**
   * Settle a provider effect. Uncertain settlement intentionally retains the
   * reservation; only a later conclusive settlement releases capacity.
   */
  settle(reservation: CapacityReservation, outcome: "settled" | "uncertain", now: number, evidenceDigest?: string): CapacityReservation {
    assertReservation(reservation);
    safeInteger(now, 0, Number.MAX_SAFE_INTEGER);
    const row = this.row(reservation.reservationId);
    if (row === null || !sameIdentity(row, reservation)) throw new Error("CAPACITY_RESERVATION_MISMATCH");
    if (row.state === "released" || row.state === "settled") return fromRow(row);
    if (outcome === "settled") {
      if (evidenceDigest === undefined) throw new Error("CAPACITY_SETTLEMENT_EVIDENCE_REQUIRED");
      evidenceDigest = boundedText(evidenceDigest, 160);
      if (evidenceDigest.length === 0) throw new Error("CAPACITY_SETTLEMENT_EVIDENCE_REQUIRED");
    }
    this.db.query(`UPDATE ${TABLE} SET state=?, settled_at=?, evidence_digest=? WHERE reservation_id=? AND owner=? AND state IN ${ACTIVE_STATES}`)
      .run(outcome, now, evidenceDigest ?? null, reservation.reservationId, reservation.owner);
    const updated = this.row(reservation.reservationId);
    if (updated === null) throw new Error("CAPACITY_RESERVATION_LOST");
    return fromRow(updated);
  }

  /** Release a reservation only when no provider effect is uncertain. */
  release(reservation: CapacityReservation): boolean {
    assertReservation(reservation);
    const row = this.row(reservation.reservationId);
    if (row === null || !sameIdentity(row, reservation)) throw new Error("CAPACITY_RESERVATION_MISMATCH");
    if (row.state === "uncertain") throw new Error("CAPACITY_UNCERTAIN_REQUIRES_SETTLEMENT");
    if (row.state !== "active") return false;
    return this.db.query(`UPDATE ${TABLE} SET state='released' WHERE reservation_id=? AND owner=? AND state='active'`)
      .run(reservation.reservationId, reservation.owner).changes === 1;
  }

  inspect(reservationId: string): CapacityReservation | null {
    return this.row(identifier(reservationId)) === null ? null : fromRow(this.row(identifier(reservationId))!);
  }

  projection(): CapacityProjection {
    const rows = this.db.query<Row>(`SELECT * FROM ${TABLE} WHERE state IN ${ACTIVE_STATES}`).all();
    let memory = 0, disk = 0, uncertain = 0;
    const accountRuns: Record<string, number> = {};
    for (const row of rows) {
      memory += row.memory_bytes; disk += row.disk_bytes;
      if (row.state === "uncertain") uncertain++;
      if (row.provider !== null && row.account_id !== null) {
        const key = accountKey(row.provider, row.account_id); accountRuns[key] = (accountRuns[key] ?? 0) + 1;
      }
    }
    return Object.freeze({ limits: this.limits, activeRuns: rows.length, uncertainRuns: uncertain,
      usedMemoryBytes: memory, usedDiskBytes: disk, availableRuns: Math.max(0, this.limits.maxRuns - rows.length),
      availableMemoryBytes: Math.max(0, this.limits.memoryBytes - memory), availableDiskBytes: Math.max(0, this.limits.diskBytes - disk),
      accountRuns: Object.freeze(accountRuns) });
  }

  private row(id: string): Row | null {
    return this.db.query<Row, [string]>(`SELECT * FROM ${TABLE} WHERE reservation_id=?`).get(id);
  }
}

function validateLimits(limits: CapacityLimits): void {
  safeInteger(limits.maxRuns, 1, 10_000); safeInteger(limits.memoryBytes, 0, Number.MAX_SAFE_INTEGER);
  safeInteger(limits.diskBytes, 0, Number.MAX_SAFE_INTEGER); safeInteger(limits.maxRunsPerAccount, 1, 10_000);
}

function normalizeRequest(value: CapacityRequest): CapacityRequest {
  if (value.class !== "foreground" && value.class !== "build" && value.class !== "maintenance" && value.class !== "recovery") throw new Error("CAPACITY_CLASS_INVALID");
  const out: CapacityRequest = Object.freeze({ reservationId: identifier(value.reservationId), owner: identifier(value.owner), workspace: identifier(value.workspace), class: value.class,
    ...(value.provider === undefined ? {} : { provider: provider(value.provider) }), ...(value.accountId === undefined ? {} : { accountId: identifier(value.accountId) }),
    memoryBytes: safeInteger(value.memoryBytes, 0, Number.MAX_SAFE_INTEGER), diskBytes: safeInteger(value.diskBytes, 0, Number.MAX_SAFE_INTEGER), createdAt: safeInteger(value.createdAt, 0, Number.MAX_SAFE_INTEGER) });
  if ((out.provider === undefined) !== (out.accountId === undefined)) throw new Error("CAPACITY_ACCOUNT_BINDING_INVALID");
  return out;
}

function assertReservation(value: CapacityReservation): void { identifier(value.reservationId); identifier(value.owner); }
function sameIdentity(row: Row, value: CapacityReservation): boolean {
  return row.owner === value.owner && row.workspace === value.workspace && row.class === value.class
    && row.provider === (value.provider ?? null) && row.account_id === (value.accountId ?? null)
    && row.memory_bytes === value.memoryBytes && row.disk_bytes === value.diskBytes
    && row.created_at === value.createdAt && row.state === value.state;
}
function assertSameRequest(row: Row, input: CapacityRequest): void {
  if (row.owner !== input.owner || row.workspace !== input.workspace || row.class !== input.class || row.provider !== (input.provider ?? null)
    || row.account_id !== (input.accountId ?? null) || row.memory_bytes !== input.memoryBytes || row.disk_bytes !== input.diskBytes || row.created_at !== input.createdAt) throw new Error("CAPACITY_RESERVATION_ID_CONFLICT");
}
function accountKey(providerValue: AgentProvider, accountId: string): string { return `${providerValue}:${accountId}`; }
function fromRow(row: Row): CapacityReservation {
  return Object.freeze({ reservationId: row.reservation_id, owner: row.owner, workspace: row.workspace, class: row.class,
    ...(row.provider === null ? {} : { provider: row.provider }), ...(row.account_id === null ? {} : { accountId: row.account_id }),
    memoryBytes: row.memory_bytes, diskBytes: row.disk_bytes, createdAt: row.created_at, state: row.state, settledAt: row.settled_at, evidenceDigest: row.evidence_digest });
}

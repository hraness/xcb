// SQLite port shared by Bun and Node runtimes. The port normalizes statement lookup,
// empty results, bindings, and close lifecycle, and lazily resolves the runtime's native
// module. Importing this file under Node never touches bun:sqlite and vice versa.

export type SqliteBinding = string | number | bigint | boolean | null | Uint8Array;
export interface SqliteStatement<Row, _Params extends SqliteBinding[] = SqliteBinding[]> {
  get(...params: unknown[]): Row | null;
  all(...params: unknown[]): Row[];
  run(...params: unknown[]): { changes: number };
}
export interface SqliteDatabase {
  exec(sql: string): void;
  query<Row = unknown, Params extends SqliteBinding[] = SqliteBinding[]>(sql: string): SqliteStatement<Row, Params>;
  close(): void;
}

type NativeStatement = {
  get(...params: never[]): unknown;
  all(...params: never[]): unknown[];
  run(...params: never[]): { changes: number | bigint };
  finalize?(): void;
};
type NativeDatabase = {
  exec(sql: string): unknown;
  prepare(sql: string): NativeStatement;
  close(): void;
};

export function wrapSqliteDatabase(database: NativeDatabase, journal: "WAL" | "DELETE" = "WAL"): SqliteDatabase {
  // Match bun:sqlite defaults so file layout and constraint behavior are identical whichever
  // runtime opened the database: WAL journal (bun default; node:sqlite leaves the file's own
  // mode) and foreign_keys off (SQLite/bun default; node:sqlite enables it by default).
  if (journal !== "WAL" && journal !== "DELETE") throw new Error("SQLITE_JOURNAL_INVALID");
  if (journal === "WAL") database.exec("PRAGMA journal_mode=WAL");
  else {
    database.exec("PRAGMA busy_timeout=0");
    const statement = database.prepare("PRAGMA journal_mode");
    try {
      const mode = statement.get() as { journal_mode?: unknown } | undefined;
      if (mode?.journal_mode !== "delete") throw new Error("SQLITE_JOURNAL_MISMATCH");
    } finally { statement.finalize?.(); }
  }
  database.exec("PRAGMA foreign_keys=OFF");
  let closed = false;
  // Bun defers closing a database while uncached prepared statements survive. Finalize
  // them explicitly so close releases transactions and file locks before it returns.
  // Weak tracking also lets temporary statements be collected in long-running sessions.
  const statements = new Set<WeakRef<NativeStatement>>();
  const collected = new FinalizationRegistry<WeakRef<NativeStatement>>(reference => statements.delete(reference));
  const assertOpen = () => { if (closed) throw new Error("SQLITE_DATABASE_CLOSED"); };
  // node:sqlite rejects boolean bindings where bun:sqlite coerces them to 0/1; normalize.
  const bind = (params: readonly unknown[]) => params.map(value => typeof value === "boolean" ? Number(value) : value);
  return {
    exec(sql) { assertOpen(); database.exec(sql); },
    query(sql) {
      assertOpen();
      const statement = database.prepare(sql);
      const reference = new WeakRef(statement);
      statements.add(reference);
      collected.register(statement, reference, reference);
      return {
        get: (...params: unknown[]) => { assertOpen(); return ((statement.get as (...args: unknown[]) => unknown)(...bind(params)) ?? null) as never; },
        all: (...params: unknown[]) => { assertOpen(); return ((statement.all as (...args: unknown[]) => unknown[])(...bind(params))) as never; },
        run: (...params: unknown[]) => { assertOpen(); return { changes: Number((statement.run as (...args: unknown[]) => { changes: number | bigint })(...bind(params)).changes) }; },
      };
    },
    close() {
      if (closed) return;
      for (const reference of statements) {
        reference.deref()?.finalize?.();
        statements.delete(reference);
        collected.unregister(reference);
      }
      database.close();
      closed = true;
    },
  };
}

export async function openAccountDatabase(path: string, journal: "WAL" | "DELETE" = "WAL"): Promise<SqliteDatabase> {
  const database: NativeDatabase = typeof Bun === "undefined"
    ? new (await import("node:sqlite")).DatabaseSync(path)
    : new (await import("bun:sqlite")).Database(path);
  try { return wrapSqliteDatabase(database, journal); }
  catch (error) { database.close(); throw error; }
}

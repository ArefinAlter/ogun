// The service's own database: one SQLite file in a volume, one row per visitor namespace. A namespace holds the
// state of the screens the daemon does not implement yet (agent threads, connections, provider settings, storage
// rules and the rest), as one JSON document. Namespaces expire after a day without a visit.

import { DatabaseSync } from 'node:sqlite'

export interface Row {
  id: string
  created_ms: number
  seen_ms: number
  data: string
}

export class Store {
  private db: DatabaseSync

  constructor(path: string) {
    this.db = new DatabaseSync(path)
    this.db.exec(`PRAGMA journal_mode = WAL;
      PRAGMA synchronous = NORMAL;
      CREATE TABLE IF NOT EXISTS ns (
        id TEXT PRIMARY KEY,
        created_ms INTEGER NOT NULL,
        seen_ms INTEGER NOT NULL,
        data TEXT NOT NULL
      );
      CREATE TABLE IF NOT EXISTS meta (k TEXT PRIMARY KEY, v TEXT NOT NULL);`)
  }

  get(id: string): Row | undefined {
    return this.db.prepare('SELECT id, created_ms, seen_ms, data FROM ns WHERE id = ?').get(id) as Row | undefined
  }

  put(id: string, data: string, now = Date.now()): void {
    this.db
      .prepare('INSERT INTO ns (id, created_ms, seen_ms, data) VALUES (?, ?, ?, ?) ON CONFLICT(id) DO UPDATE SET seen_ms = excluded.seen_ms, data = excluded.data')
      .run(id, now, now, data)
  }

  touch(id: string, now = Date.now()): void {
    this.db.prepare('UPDATE ns SET seen_ms = ? WHERE id = ?').run(now, id)
  }

  count(): number {
    return (this.db.prepare('SELECT COUNT(*) AS n FROM ns').get() as { n: number }).n
  }

  /** Deletes namespaces not seen since `before_ms`; returns how many. */
  expire(before_ms: number): number {
    return Number(this.db.prepare('DELETE FROM ns WHERE seen_ms < ?').run(before_ms).changes)
  }

  /** Deletes everything (the nightly reset). */
  clear(): void {
    this.db.exec('DELETE FROM ns')
  }

  meta(k: string): string | undefined {
    return (this.db.prepare('SELECT v FROM meta WHERE k = ?').get(k) as { v: string } | undefined)?.v
  }

  setMeta(k: string, v: string): void {
    this.db.prepare('INSERT INTO meta (k, v) VALUES (?, ?) ON CONFLICT(k) DO UPDATE SET v = excluded.v').run(k, v)
  }
}

/** JSON with Sets and Maps kept. */
export function serialize(v: unknown): string {
  return JSON.stringify(v, (_k, x) => {
    if (x instanceof Set) return { __set: [...x] }
    if (x instanceof Map) return { __map: [...x.entries()] }
    return x
  })
}

export function deserialize<T>(s: string): T {
  return JSON.parse(s, (_k, x) => {
    if (x && typeof x === 'object' && !Array.isArray(x)) {
      if ('__set' in x && Object.keys(x).length === 1) return new Set(x.__set)
      if ('__map' in x && Object.keys(x).length === 1) return new Map(x.__map)
    }
    return x
  }) as T
}

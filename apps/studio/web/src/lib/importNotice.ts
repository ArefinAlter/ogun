// The result of the last sample import, kept in memory so the Workflows page can say what was stored after the
// first-run panel navigates there. Nothing is persisted.

import type { SampleImportResult } from '../api/types'

let last: SampleImportResult | null = null

export function lastImport(): SampleImportResult | null {
  return last
}

export function setLastImport(r: SampleImportResult | null): void {
  last = r
}

// Paged results and streaming exports for Forge.
//
// A Forge query can return far more documents than the app can draw. Instead of sending the
// whole result over stdio, the sidecar keeps the result here and hands the app one page at a
// time. Exports read the same way, a chunk at a time, so the app never holds the full result.

const SHELL_API_TYPE = Symbol.for("@@mongosh.shellApiType");

// Cursors mongosh can page through. Change streams never end and explain cursors print a plan,
// so neither is paged.
const PAGEABLE_CURSOR_TYPES = new Set(["Cursor", "AggregationCursor", "RunCommandCursor"]);

// Kept per session; older results are closed when a newer one pushes them out.
export const MAX_RESULTS_PER_SESSION = 8;

// Documents a cursor result keeps for paging back. Paging forward past this drops the oldest
// ones; paging back to them asks for the query to be run again. An array result is already all
// in memory, so it is never trimmed.
export const MAX_CACHED_DOCUMENTS = 50_000;

export type Paging = {
  result_id: string;
  page: number;
  page_size: number;
  // Index of the first document on this page within the whole result.
  offset: number;
  count: number;
  has_more: boolean;
  // Known once the result has been read to its end, or when it was an array to begin with.
  total: number | null;
};

type ShellCursor = {
  tryNext(): Promise<unknown>;
  close?: () => Promise<void> | void;
};

export type ResultSource = {
  id: string;
  // Documents read so far, in order, starting at `base`. An array result holds everything.
  cache: unknown[];
  base: number;
  cursor: ShellCursor | null;
  exhausted: boolean;
  limit: number;
  // Result pages in the app showing this result; it is closed when the last one lets go.
  holders: number;
};

export function shellApiType(value: unknown): string | null {
  if (value === null || (typeof value !== "object" && typeof value !== "function")) return null;
  const type = (value as Record<symbol, unknown>)[SHELL_API_TYPE];
  return typeof type === "string" ? type : null;
}

export function isPageableCursor(value: unknown): value is ShellCursor {
  const type = shellApiType(value);
  return (
    type !== null &&
    PAGEABLE_CURSOR_TYPES.has(type) &&
    typeof (value as ShellCursor).tryNext === "function"
  );
}

// A document is a plain object. BSON values (Int32, Long, ObjectId…) are objects too, and so are
// mongosh's result wrappers, so both are excluded by name.
export function isPlainDocument(value: unknown): boolean {
  if (value === null || typeof value !== "object" || Array.isArray(value)) return false;
  if (shellApiType(value) !== null || "_bsontype" in (value as object)) return false;
  const prototype = Object.getPrototypeOf(value);
  return prototype === Object.prototype || prototype === null;
}

// Only arrays of documents are paged. An array of numbers or strings is a value to read, not a
// result set, and keeps printing as it did.
export function isDocumentArray(value: unknown): value is unknown[] {
  return Array.isArray(value) && value.length > 0 && value.every(isPlainDocument);
}

export function validPageSize(value: unknown): number | null {
  return typeof value === "number" && Number.isInteger(value) && value > 0 && value <= 100_000
    ? value
    : null;
}

async function closeCursor(cursor: ShellCursor | null) {
  try {
    await cursor?.close?.();
  } catch {
    // The server may already have dropped it; nothing is left to release.
  }
}

function readEnd(source: ResultSource) {
  return source.base + source.cache.length;
}

// Read until `wanted` documents have been read in total, or the cursor runs out. The window
// keeps at least `keepFrom` onward, so the page being read is never trimmed away.
async function fill(source: ResultSource, wanted: number, keepFrom: number) {
  while (!source.exhausted && readEnd(source) < wanted) {
    const doc = await source.cursor!.tryNext();
    if (doc === null || doc === undefined) {
      source.exhausted = true;
      await closeCursor(source.cursor);
      source.cursor = null;
    } else {
      source.cache.push(doc);
      const excess = source.cache.length - source.limit;
      const trimmable = Math.min(excess, keepFrom - source.base);
      if (trimmable > 0) {
        source.cache.splice(0, trimmable);
        source.base += trimmable;
      }
    }
  }
}

function pageOf(source: ResultSource, page: number, pageSize: number) {
  const offset = page * pageSize;
  if (offset < source.base) {
    throw new Error(
      `Only the latest ${source.limit.toLocaleString("en-US")} documents are kept for paging back. Run the query again to see page ${page + 1}.`,
    );
  }
  const start = offset - source.base;
  const documents = source.cache.slice(start, start + pageSize);
  const paging: Paging = {
    result_id: source.id,
    page,
    page_size: pageSize,
    offset,
    count: documents.length,
    has_more: readEnd(source) > offset + pageSize,
    total: source.exhausted ? readEnd(source) : null,
  };
  return { documents, paging };
}

export async function readPage(source: ResultSource, page: number, pageSize: number) {
  const offset = page * pageSize;
  // One past the page tells us whether a next page exists without a second round trip.
  await fill(source, offset + pageSize + 1, offset);
  return pageOf(source, page, pageSize);
}

// The last page needs the total, so the cursor is read to its end. Only the final page is kept
// from what that reads past the window.
export async function readLastPage(source: ResultSource, pageSize: number) {
  while (!source.exhausted) {
    const end = readEnd(source);
    const lastStart = Math.floor(end / pageSize) * pageSize;
    await fill(source, end + pageSize, Math.max(source.base, lastStart - pageSize));
  }
  const total = readEnd(source);
  const page = Math.max(0, Math.ceil(total / pageSize) - 1);
  return pageOf(source, page, pageSize);
}

export class ResultRegistry {
  private results = new Map<string, ResultSource>();
  private nextId = 1;
  // A cursor kept in a shell variable can be the result of more than one run. Two owners would
  // each read part of it and show pages with documents missing, so a cursor has one owner.
  private owners = new WeakMap<object, string>();

  // The result that already holds this cursor, if it is still open.
  ownerOf(value: unknown): ResultSource | null {
    if (value === null || typeof value !== "object") return null;
    const id = this.owners.get(value);
    return id ? (this.results.get(id) ?? null) : null;
  }

  get size() {
    return this.results.size;
  }

  // Keep an unread cursor or a document array for paging.
  async register(
    value: unknown,
    limit = MAX_CACHED_DOCUMENTS,
  ): Promise<ResultSource | null> {
    let source: ResultSource;
    const id = `r${this.nextId++}`;
    if (isPageableCursor(value)) {
      const owner = this.ownerOf(value);
      if (owner) {
        owner.holders += 1;
        return owner;
      }
      source = { id, cache: [], base: 0, cursor: value, exhausted: false, limit, holders: 1 };
      this.owners.set(value, id);
    } else if (isDocumentArray(value)) {
      source = {
        id,
        cache: value,
        base: 0,
        cursor: null,
        exhausted: true,
        limit: Number.MAX_SAFE_INTEGER,
        holders: 1,
      };
    } else {
      return null;
    }
    this.results.set(source.id, source);
    while (this.results.size > MAX_RESULTS_PER_SESSION) {
      const oldest = this.results.keys().next().value as string;
      await this.close(oldest);
    }
    return source;
  }

  get(id: string): ResultSource {
    const source = this.results.get(id);
    if (!source) {
      throw new Error(
        "This result is no longer available. Run the query again to page through it.",
      );
    }
    return source;
  }

  // One result page let go of this result.
  async release(id: string) {
    const source = this.results.get(id);
    if (source && --source.holders <= 0) {
      await this.close(id);
    }
  }

  private async close(id: string) {
    const source = this.results.get(id);
    this.results.delete(id);
    await closeCursor(source?.cursor ?? null);
  }

  async clear() {
    for (const id of [...this.results.keys()]) {
      await this.close(id);
    }
  }
}

// An export reads the whole result once, in chunks, and wraps anything that is not a document
// as `{ value }`, the way the results view shows it.
export type ExportSource = {
  id: string;
  pending: unknown[];
  cursor: ShellCursor | null;
  read: number;
};

export function exportSourceFor(id: string, value: unknown): ExportSource {
  if (isPageableCursor(value)) {
    return { id, pending: [], cursor: value, read: 0 };
  }
  if (Array.isArray(value)) {
    return { id, pending: [...value], cursor: null, read: 0 };
  }
  if (value === null || value === undefined) {
    return { id, pending: [], cursor: null, read: 0 };
  }
  if (isPlainDocument(value)) {
    return { id, pending: [value], cursor: null, read: 0 };
  }
  const type = shellApiType(value) ?? typeof value;
  throw new Error(
    `Export to Excel needs a query that returns documents, such as find() or aggregate(). This one returned ${type}.`,
  );
}

function asDocument(value: unknown): unknown {
  return isPlainDocument(value) ? value : { value };
}

export async function readExportChunk(source: ExportSource, max: number) {
  const documents: unknown[] = [];
  while (documents.length < max && source.pending.length > 0) {
    documents.push(asDocument(source.pending.shift()));
  }
  while (documents.length < max && source.cursor) {
    const doc = await source.cursor.tryNext();
    if (doc === null || doc === undefined) {
      await closeCursor(source.cursor);
      source.cursor = null;
    } else {
      documents.push(asDocument(doc));
    }
  }
  source.read += documents.length;
  return { documents, done: source.pending.length === 0 && source.cursor === null };
}

export async function closeExport(source: ExportSource | undefined) {
  if (source) {
    source.pending = [];
    await closeCursor(source.cursor);
    source.cursor = null;
  }
}

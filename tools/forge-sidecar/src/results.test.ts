import { expect, test } from "bun:test";
import { Int32, Long, ObjectId } from "bson";
import {
  ResultRegistry,
  exportSourceFor,
  isDocumentArray,
  isPlainDocument,
  readExportChunk,
  readLastPage,
  readPage,
  validPageSize,
} from "./results";

const SHELL_API_TYPE = Symbol.for("@@mongosh.shellApiType");

// A stand-in for a mongosh cursor: tagged like one, and it counts what was read from it.
function fakeCursor(total: number) {
  let next = 0;
  const cursor = {
    [SHELL_API_TYPE]: "Cursor",
    reads: 0,
    closed: false,
    async tryNext() {
      cursor.reads += 1;
      return next < total ? { _id: next++ } : null;
    },
    async close() {
      cursor.closed = true;
    },
  };
  return cursor;
}

const ids = (documents: unknown[]) => documents.map((doc) => (doc as { _id: number })._id);

test("a cursor is read one page at a time, not to its end", async () => {
  const cursor = fakeCursor(18_000);
  const source = (await new ResultRegistry().register(cursor))!;

  const first = await readPage(source, 0, 1000);
  expect(ids(first.documents)).toEqual([...Array(1000).keys()]);
  expect(first.paging).toMatchObject({ page: 0, offset: 0, count: 1000, has_more: true, total: null });
  // One document past the page, to know whether there is a next one.
  expect(cursor.reads).toBe(1001);

  const second = await readPage(source, 1, 1000);
  expect(ids(second.documents)[0]).toBe(1000);
  expect(cursor.reads).toBe(2001);

  // Going back is served from what was already read.
  await readPage(source, 0, 1000);
  expect(cursor.reads).toBe(2001);
});

test("the last page reads to the end and reports the total", async () => {
  const cursor = fakeCursor(2_345);
  const source = (await new ResultRegistry().register(cursor))!;
  const last = await readLastPage(source, 1000);
  expect(last.paging).toMatchObject({ page: 2, offset: 2000, count: 345, has_more: false, total: 2345 });
  expect(cursor.closed).toBe(true);
});

test("paging far forward keeps a bounded window and says why older pages are gone", async () => {
  const source = (await new ResultRegistry().register(fakeCursor(10_000), 2_500))!;
  const page = await readPage(source, 8, 1000);
  expect(ids(page.documents)[0]).toBe(8000);
  expect(source.cache.length).toBeLessThanOrEqual(2_500);
  expect(readPage(source, 0, 1000)).rejects.toThrow("Run the query again to see page 1");
  const last = await readLastPage(source, 1000);
  expect(last.paging.total).toBe(10_000);
});

test("an array of documents pages; values and BSON scalars do not", async () => {
  const registry = new ResultRegistry();
  const docs = Array.from({ length: 2500 }, (_, i) => ({ _id: i }));
  const source = (await registry.register(docs))!;
  expect((await readPage(source, 2, 1000)).paging).toMatchObject({ count: 500, total: 2500 });

  expect(await registry.register([1, 2, 3])).toBeNull();
  expect(await registry.register([new Int32(1), new Int32(2)])).toBeNull();
  expect(await registry.register([])).toBeNull();
  expect(await registry.register(new Int32(18000))).toBeNull();
  expect(isDocumentArray([{ a: 1 }, new ObjectId()])).toBe(false);
  expect(isPlainDocument(new Long(1))).toBe(false);
  expect(isPlainDocument({ _id: new ObjectId() })).toBe(true);
});

test("a cursor shown twice is one result, so neither owner loses documents", async () => {
  const registry = new ResultRegistry();
  const cursor = fakeCursor(26);
  const first = (await registry.register(cursor))!;
  expect(ids((await readPage(first, 0, 5)).documents)).toEqual([0, 1, 2, 3, 4]);
  // Running `shared` again hands back the same cursor object.
  const second = (await registry.register(cursor))!;
  expect(second.id).toBe(first.id);
  expect(ids((await readPage(second, 0, 5)).documents)).toEqual([0, 1, 2, 3, 4]);
  expect(ids((await readPage(first, 1, 5)).documents)).toEqual([5, 6, 7, 8, 9]);
  expect((await readLastPage(first, 5)).paging.total).toBe(26);

  // Closing one of the two pages keeps the result for the other.
  await registry.release(first.id);
  expect(registry.get(first.id)).toBe(first);
  await registry.release(first.id);
  expect(() => registry.get(first.id)).toThrow("Run the query again");
});

test("older results are closed when a session keeps too many", async () => {
  const registry = new ResultRegistry();
  const first = fakeCursor(10);
  const firstSource = (await registry.register(first))!;
  for (let i = 0; i < 8; i++) {
    await registry.register(fakeCursor(10));
  }
  expect(first.closed).toBe(true);
  expect(() => registry.get(firstSource.id)).toThrow("Run the query again");
});

test("an export reads a cursor in chunks and wraps values that are not documents", async () => {
  const cursor = fakeCursor(12_001);
  const source = exportSourceFor("e1", cursor);
  let total = 0;
  let done = false;
  while (!done) {
    const chunk = await readExportChunk(source, 5000);
    expect(chunk.documents.length).toBeLessThanOrEqual(5000);
    total += chunk.documents.length;
    done = chunk.done;
  }
  expect(total).toBe(12_001);
  expect(cursor.closed).toBe(true);

  const values = await readExportChunk(exportSourceFor("e2", [1, { a: 1 }]), 10);
  expect(values).toEqual({ documents: [{ value: 1 }, { a: 1 }], done: true });
});

test("an export refuses a result that is not documents", () => {
  expect(() => exportSourceFor("e", new Int32(18000))).toThrow("needs a query that returns documents");
  expect(() => exportSourceFor("e", 5)).toThrow("returned number");
  expect(() => exportSourceFor("e", { [SHELL_API_TYPE]: "InsertOneResult" })).toThrow(
    "returned InsertOneResult",
  );
});

test("page sizes are bounded whole numbers", () => {
  expect(validPageSize(1000)).toBe(1000);
  for (const bad of [0, -1, 1.5, "1000", null, 100_001]) {
    expect(validPageSize(bad)).toBeNull();
  }
});

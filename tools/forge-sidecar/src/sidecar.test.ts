import { expect, test } from "bun:test";
import { spawn } from "node:child_process";
import { createInterface } from "node:readline";

// Use a disposable database. This suite never chooses a saved app connection.
const binary = process.env.FORGE_SIDECAR_BINARY;
const uri = process.env.FORGE_TEST_MONGODB_URI;

async function withSidecar(
  check: (request: (method: string, params?: object, timeoutMs?: number) => Promise<any>) => Promise<void>,
) {
  const child = spawn(binary!, [], { stdio: ["pipe", "pipe", "pipe"] });
  const output = createInterface({ input: child.stdout });
  const lines = output[Symbol.asyncIterator]();
  let errors = "";
  child.stderr.on("data", (data) => { errors = (errors + data).slice(-4000); });
  let id = 0;
  try {
    await check(async (method, params = {}, timeoutMs = 10_000) => {
      const requestId = ++id;
      const events = [];
      child.stdin.write(JSON.stringify({ id: requestId, method, params }) + "\n");
      let timer: ReturnType<typeof setTimeout>;
      try {
        return await Promise.race([
          (async () => {
            while (true) {
              const line = await lines.next();
              if (line.done) throw new Error(`Sidecar exited: ${errors}`);
              const message = JSON.parse(line.value);
              if (message.event) {
                events.push(message);
              } else if (message.id === requestId) {
                return { ...message, events };
              }
            }
          })(),
          new Promise((_, reject) => {
            timer = setTimeout(() => reject(new Error(`${method} timed out: ${errors}`)), timeoutMs);
          }),
        ]);
      } finally {
        clearTimeout(timer!);
      }
    });
  } finally {
    output.close();
    child.kill();
    await new Promise<void>((resolve) => child.once("close", () => resolve()));
  }
}

test.skipIf(!binary)("compiled sidecar answers RPC and reports errors", async () => {
  await withSidecar(async (request) => {
    expect(await request("ping")).toMatchObject({ ok: true, result: "pong" });
    expect(await request("unknown_method")).toMatchObject({ ok: false });
    expect(await request("evaluate", { session_id: "missing", code: "1" }))
      .toMatchObject({ ok: false, error: "Session not found: missing" });
  });
});

test.skipIf(!binary || !uri)("compiled shell preserves BSON, completion, output and sessions beyond 30 seconds", async () => {
  await withSidecar(async (request) => {
    for (const session_id of ["idle", "active"]) {
      expect(await request("create_session", { session_id, uri, database: "forge_sidecar_test" }))
        .toMatchObject({ ok: true });
      // Canonical Extended JSON: the fork's editable results need Int32 to stay Int32.
      expect(await request("evaluate", { session_id, code: "const kept = 42; kept" }))
        .toMatchObject({ ok: true, result: { printable: { $numberInt: "42" } } });
    }

    const complete = await request("complete", { session_id: "active", code: "db.getCol" });
    expect(complete.ok).toBe(true);
    expect(complete.result).toContainEqual({ completion: "db.getCollection" });

    const printed = await request("evaluate", {
      session_id: "active", run_id: 7,
      code: 'print("hello"); printjson({ id: ObjectId("507f1f77bcf86cd799439011"), when: ISODate("2020-01-01") });',
    });
    expect(printed).toMatchObject({ ok: true, result: { is_undefined: true } });
    expect(printed.events).toHaveLength(2);
    expect(printed.events[0]).toMatchObject({ run_id: 7, lines: ["hello"] });
    expect(printed.events[1].payload).toEqual([{
      id: { $oid: "507f1f77bcf86cd799439011" },
      when: { $date: { $numberLong: "1577836800000" } },
    }]);
    expect(await request("evaluate", { session_id: "active", code: "null" }))
      .toMatchObject({ ok: true, result: { printable: null, is_undefined: false } });

    // The old global idle timer closed both the running query and the idle shell.
    expect(await request("evaluate", {
      session_id: "active", code: "(async () => { await sleep(31_000); return kept + (await db.runCommand({ ping: 1 })).ok; })()",
    }, 40_000)).toMatchObject({ ok: true, result: { printable: { $numberInt: "43" } } });
    expect(await request("evaluate", { session_id: "idle", code: "kept" }))
      .toMatchObject({ ok: true, result: { printable: { $numberInt: "42" } } });

    for (const session_id of ["idle", "active"]) {
      expect(await request("dispose_session", { session_id })).toMatchObject({ ok: true });
      expect(await request("evaluate", { session_id, code: "kept" }))
        .toMatchObject({ ok: false, error: `Session not found: ${session_id}` });
    }
  });
}, 60_000);

test.skipIf(!binary || !uri)("compiled shell pages a large result and exports it in chunks", async () => {
  await withSidecar(async (request) => {
    const session_id = "paging";
    expect(await request("create_session", { session_id, uri, database: "forge_sidecar_test" }))
      .toMatchObject({ ok: true });
    await request("evaluate", {
      session_id,
      code: "db.paged.drop(); db.paged.insertMany(Array.from({ length: 2500 }, (_, i) => ({ _id: i })))",
    });

    // Without a page size, mongosh prints the first batch of 20 as it always has.
    const printed = await request("evaluate", { session_id, code: "db.paged.find().sort({ _id: 1 })" });
    expect(printed.result.printable.documents).toHaveLength(20);
    expect(printed.result.paging).toBeUndefined();

    const first = await request("evaluate", {
      session_id, code: "db.paged.find().sort({ _id: 1 }).toArray()", page_size: 1000,
    });
    expect(first.result.printable).toHaveLength(1000);
    expect(first.result.paging).toMatchObject({ page: 0, has_more: true, total: 2500 });

    const last = await request("page", {
      session_id, result_id: first.result.paging.result_id, page: "last", page_size: 1000,
    });
    expect(last.result.printable).toHaveLength(500);
    expect(last.result.printable[0]).toEqual({ _id: { $numberInt: "2000" } });

    const opened = await request("export_open", { session_id, code: "db.paged.find()" });
    let exported = 0;
    for (let done = false; !done;) {
      const chunk = await request("export_next", {
        session_id, export_id: opened.result.export_id, max: 1000,
      });
      exported += chunk.result.documents.length;
      done = chunk.result.done;
    }
    expect(exported).toBe(2500);

    await request("evaluate", { session_id, code: "db.paged.drop()" });
    expect(await request("dispose_session", { session_id })).toMatchObject({ ok: true });
  });
}, 60_000);

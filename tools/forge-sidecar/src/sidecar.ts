import { ElectronRuntime } from "@mongosh/browser-runtime-electron";
import { CompassServiceProvider } from "@mongosh/service-provider-node-driver";
import { formatPrintValue, safePrintable } from "./format";
import {
  ResultRegistry,
  closeExport,
  exportSourceFor,
  isPageableCursor,
  readExportChunk,
  readLastPage,
  readPage,
  shellApiType,
  validPageSize,
  type ExportSource,
} from "./results";
import { EventEmitter } from "events";
import readline from "readline";

// How the next evaluation's result is handled. "print" is mongosh's own behaviour. "page" keeps
// a cursor unread so the app can page through it. "capture" keeps the raw value for an export
// without reading anything from it.
type ResultMode = "print" | "page" | "capture";

type Session = {
  runtime: ElectronRuntime;
  provider: CompassServiceProvider;
  uri: string;
  database: string;
  currentRunId?: number | null;
  mode: ResultMode;
  captured: { value: unknown } | null;
  // False when this mongosh build does not expose the evaluator; paging and export then say so.
  canCapture: boolean;
  results: ResultRegistry;
  exports: Map<string, ExportSource>;
  nextExportId: number;
  // Requests are read concurrently, but `mode` and `captured` belong to one evaluation at a
  // time, and a cursor must not be read by two requests at once. Work on a session queues here.
  queue: Promise<unknown>;
};

function serialized<T>(session: Session, work: () => Promise<T>): Promise<T> {
  const run = session.queue.then(work, work);
  session.queue = run.catch(() => undefined);
  return run;
}

type RequestMessage = {
  id: number;
  method: string;
  params?: Record<string, unknown>;
};

type ResponseMessage = {
  id: number;
  ok: boolean;
  result?: unknown;
  error?: string;
};

const sessions = new Map<string, Session>();

// Sessions belong to Forge tabs. The host disposes them when tabs close or restart;
// inactivity must not discard shell variables or interrupt an evaluation.

function send(message: ResponseMessage) {
  process.stdout.write(`${JSON.stringify(message)}\n`);
}

function sendEvent(message: Record<string, unknown>) {
  process.stdout.write(`${JSON.stringify(message)}\n`);
}

function requireSession(sessionId: string): Session {
  const session = sessions.get(sessionId);
  if (!session) {
    throw new Error(`Session not found: ${sessionId}`);
  }
  return session;
}

function splitLines(text: string): string[] {
  return text.split(/\r?\n/);
}

async function createSession(params: Record<string, unknown>) {
  const sessionId = params.session_id as string | undefined;
  const uri = params.uri as string | undefined;
  const database = (params.database as string | undefined) ?? "";
  const driverOptions = (params.driver_options as Record<string, unknown> | undefined) ?? {};

  if (!sessionId) {
    throw new Error("create_session missing session_id");
  }
  if (!uri) {
    throw new Error("create_session missing uri");
  }

  if (sessions.has(sessionId)) {
    await disposeSession({ session_id: sessionId });
  }

  const bus = new EventEmitter();
  const provider = await CompassServiceProvider.connect(
    uri,
    {
      productName: "OpenMango",
      productDocsLink: "https://github.com/ggagosh/openmango",
      appName: "OpenMango",
      // Editable result views compare the original BSON value before applying an
      // update. Keep the driver's BSON wrappers instead of promoting integral
      // doubles and other numeric values to plain JavaScript numbers.
      promoteValues: false,
      promoteLongs: false,
      ...driverOptions,
    },
    {},
    bus
  );

  const runtime = new ElectronRuntime(provider, bus);

  runtime.setEvaluationListener({
    onPrint(values: any[], type?: string) {
      const kind =
        typeof type === "string" && type.toLowerCase().includes("json")
          ? "printjson"
          : "print";
      const rendered = values
        .map((value) => formatPrintValue(value, kind))
        .join(kind === "print" ? " " : "\n");
      const lines = splitLines(rendered);
      const runId = sessions.get(sessionId)?.currentRunId ?? null;
      const payload =
        kind === "printjson"
          ? values.map((value) =>
              safePrintable(
                value && typeof value === "object" && "printable" in value
                  ? (value as any).printable
                  : value
              )
            )
          : undefined;
      sendEvent({
        event: "print",
        session_id: sessionId,
        run_id: runId,
        kind,
        lines,
        payload,
      });
    },
    onClearCommand() {
      sendEvent({
        event: "clear",
        session_id: sessionId,
      });
    },
  });

  if (database) {
    await runtime.evaluate(`db = db.getSiblingDB(${JSON.stringify(database)})`);
  }

  const session: Session = {
    runtime,
    provider,
    uri,
    database,
    currentRunId: null,
    mode: "print",
    captured: null,
    canCapture: false,
    results: new ResultRegistry(),
    exports: new Map(),
    nextExportId: 1,
    queue: Promise.resolve(),
  };
  session.canCapture = installResultCapture(session);
  sessions.set(sessionId, session);
  return true;
}

// mongosh turns the value of an evaluation into printable output in its evaluator's result
// handler, and for a cursor that means reading its first batch. Wrapping the handler lets a paged
// run or an export take the cursor before anything is read from it.
function installResultCapture(session: Session): boolean {
  const evaluator = (session.runtime as any).openContextRuntime?.shellEvaluator;
  if (!evaluator || typeof evaluator.resultHandler !== "function") {
    console.error("[forge-sidecar] mongosh evaluator not found; paging and export are off");
    return false;
  }
  const original = evaluator.resultHandler;
  evaluator.resultHandler = async (raw: unknown) => {
    if (session.mode === "print") {
      return await original(raw);
    }
    let value = raw;
    if (value && typeof (value as any).then === "function") {
      value = await value;
    }
    session.captured = { value };
    if (session.mode === "capture" || isPageableCursor(value)) {
      return { type: shellApiType(value), rawValue: value, printable: undefined };
    }
    return await original(value);
  };
  return true;
}

async function resetDatabase(session: Session) {
  // Forge tabs are database-scoped. A previous query may have reassigned the
  // mongosh global `db`, so restore the tab database before every evaluation.
  // This keeps structured-result provenance and subsequent field edits bound to
  // the database shown by OpenMango rather than stale shell state.
  if (session.database) {
    await session.runtime.evaluate(
      `db = db.getSiblingDB(${JSON.stringify(session.database)})`
    );
  }
}

async function releaseSessionResults(session: Session) {
  await session.results.clear();
  for (const source of session.exports.values()) {
    await closeExport(source);
  }
  session.exports.clear();
}

async function disposeSession(params: Record<string, unknown>) {
  const sessionId = params.session_id as string | undefined;
  if (!sessionId) {
    throw new Error("dispose_session missing session_id");
  }

  const session = sessions.get(sessionId);
  if (!session) {
    return false;
  }

  sessions.delete(sessionId);
  await releaseSessionResults(session);
  await session.provider.close();
  return true;
}

async function resetSession(params: Record<string, unknown>) {
  const sessionId = params.session_id as string | undefined;
  if (!sessionId) {
    throw new Error("reset_session missing session_id");
  }

  const session = sessions.get(sessionId);
  if (!session) {
    throw new Error(`Session not found: ${sessionId}`);
  }

  await disposeSession({ session_id: sessionId });
  return await createSession({
    session_id: sessionId,
    uri: session.uri,
    database: session.database,
  });
}

async function evaluate(params: Record<string, unknown>) {
  const sessionId = params.session_id as string | undefined;
  const code = params.code as string | undefined;
  const runId = typeof params.run_id === "number" ? params.run_id : null;
  if (!sessionId || code === undefined) {
    throw new Error("evaluate missing session_id or code");
  }

  const session = requireSession(sessionId);
  return await serialized(session, () => evaluateNow(session, code, runId, params.page_size));
}

async function evaluateNow(
  session: Session,
  code: string,
  runId: number | null,
  requestedPageSize: unknown,
) {
  // With a page size, a cursor or an array of documents comes back one page at a time and the
  // rest stays here for the `page` request. Without one, results print as mongosh prints them.
  const pageSize = session.canCapture ? validPageSize(requestedPageSize) : null;
  await resetDatabase(session);
  session.currentRunId = runId;
  session.mode = pageSize ? "page" : "print";
  session.captured = null;
  try {
    const result = await session.runtime.evaluate(code);
    const captured = session.captured as { value: unknown } | null;
    if (pageSize && captured) {
      const source = await session.results.register(captured.value);
      if (source) {
        const { documents, paging } = await readPage(source, 0, pageSize);
        const printable = isPageableCursor(captured.value)
          ? { documents, cursorHasMore: paging.has_more }
          : documents;
        return {
          type: result.type,
          source: result.source,
          run_id: runId,
          is_undefined: false,
          printable: safePrintable(printable),
          paging,
        };
      }
    }
    return {
      ...result,
      run_id: runId,
      is_undefined: result.printable === undefined,
      printable: safePrintable(result.printable),
    };
  } finally {
    session.mode = "print";
    session.captured = null;
    session.currentRunId = null;
  }
}

function requirePageSize(value: unknown): number {
  const pageSize = validPageSize(value);
  if (!pageSize) {
    throw new Error("page_size must be a whole number from 1 to 100000");
  }
  return pageSize;
}

async function page(params: Record<string, unknown>) {
  const session = requireSession(params.session_id as string);
  return await serialized(session, () => pageNow(session, params));
}

async function pageNow(session: Session, params: Record<string, unknown>) {
  const source = session.results.get(params.result_id as string);
  const pageSize = requirePageSize(params.page_size);
  const pageIndex = params.page;
  let result;
  if (pageIndex === "last") {
    result = await readLastPage(source, pageSize);
  } else if (typeof pageIndex === "number" && Number.isInteger(pageIndex) && pageIndex >= 0) {
    result = await readPage(source, pageIndex, pageSize);
  } else {
    throw new Error("page must be a whole number from 0, or \"last\"");
  }
  return { printable: safePrintable(result.documents), paging: result.paging };
}

async function releaseResult(params: Record<string, unknown>) {
  const session = sessions.get(params.session_id as string);
  if (session) {
    await serialized(session, () => session.results.release(params.result_id as string));
  }
  return true;
}

// Run code for an export: the value is kept as it is, and nothing is read or printed from it.
async function exportOpen(params: Record<string, unknown>) {
  const session = requireSession(params.session_id as string);
  return await serialized(session, () => exportOpenNow(session, params));
}

async function exportOpenNow(session: Session, params: Record<string, unknown>) {
  const code = params.code;
  if (typeof code !== "string") {
    throw new Error("export_open missing code");
  }
  if (!session.canCapture) {
    throw new Error("Export to Excel is not available in this Forge runtime.");
  }
  await resetDatabase(session);
  session.mode = "capture";
  session.captured = null;
  let value: unknown;
  try {
    await session.runtime.evaluate(code);
    value = (session.captured as { value: unknown } | null)?.value;
  } finally {
    session.mode = "print";
    session.captured = null;
  }
  // A cursor a paged result is still reading can't also be exported: each would get part of it.
  if (session.results.ownerOf(value)) {
    throw new Error(
      "This cursor is already open in the results. Export the query itself, e.g. db.coll.find(...), rather than a variable holding it.",
    );
  }
  for (const source of session.exports.values()) {
    if (source.cursor !== null && source.cursor === value) {
      throw new Error("This cursor is already being exported.");
    }
  }
  const id = `e${session.nextExportId++}`;
  session.exports.set(id, exportSourceFor(id, value));
  return { export_id: id, type: shellApiType(value) };
}

async function exportNext(params: Record<string, unknown>) {
  const session = requireSession(params.session_id as string);
  return await serialized(session, () => exportNextNow(session, params));
}

async function exportNextNow(session: Session, params: Record<string, unknown>) {
  const id = params.export_id as string;
  const source = session.exports.get(id);
  if (!source) {
    throw new Error("This export is no longer open.");
  }
  const max = Math.min(requirePageSize(params.max), 10_000);
  const { documents, done } = await readExportChunk(source, max);
  if (done) {
    session.exports.delete(id);
  }
  return { documents: safePrintable(documents), done };
}

async function exportClose(params: Record<string, unknown>) {
  const session = sessions.get(params.session_id as string);
  const id = params.export_id as string;
  if (session) {
    await serialized(session, async () => {
      const source = session.exports.get(id);
      session.exports.delete(id);
      await closeExport(source);
    });
  }
  return true;
}

async function complete(params: Record<string, unknown>) {
  const sessionId = params.session_id as string | undefined;
  const code = params.code as string | undefined;
  if (!sessionId || code === undefined) {
    throw new Error("complete missing session_id or code");
  }

  const session = requireSession(sessionId);
  return await session.runtime.getCompletions(code);
}

async function handleRequest(req: RequestMessage) {
  switch (req.method) {
    case "create_session":
      return await createSession(req.params ?? {});
    case "dispose_session":
      return await disposeSession(req.params ?? {});
    case "reset_session":
      return await resetSession(req.params ?? {});
    case "evaluate":
      return await evaluate(req.params ?? {});
    case "complete":
      return await complete(req.params ?? {});
    case "page":
      return await page(req.params ?? {});
    case "release_result":
      return await releaseResult(req.params ?? {});
    case "export_open":
      return await exportOpen(req.params ?? {});
    case "export_next":
      return await exportNext(req.params ?? {});
    case "export_close":
      return await exportClose(req.params ?? {});
    case "ping":
      return "pong";
    default:
      throw new Error(`Unknown method: ${req.method}`);
  }
}

const rl = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });

rl.on("line", async (line) => {
  const trimmed = line.trim();
  if (!trimmed) return;

  let request: RequestMessage;
  try {
    request = JSON.parse(trimmed);
  } catch (err) {
    console.error("[forge-sidecar] Invalid JSON:", err);
    return;
  }

  if (typeof request.id !== "number" || !request.method) {
    console.error("[forge-sidecar] Invalid request:", request);
    return;
  }

  try {
    const result = await handleRequest(request);
    send({ id: request.id, ok: true, result });
  } catch (err) {
    send({
      id: request.id,
      ok: false,
      error: err instanceof Error ? err.message : String(err),
    });
  }
});

rl.on("close", () => {
  process.exit(0);
});

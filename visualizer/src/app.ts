type Area = {
  e: string;
  start: number;
  end: number;
  perms: string;
  path: string;
  skip?: boolean;
  pages?: PageRun[];
  pagemap?: PagemapRun[];
  pagemapError?: boolean;
  sharedFdInitial?: SharedFdInitial;
};

type PageRun = { index: number; count: number; flag: number };
type PagemapRun = { index: number; count: number; bits: number };
type SharedFdInitial = { fd: number | null; nextHole: number | null; nextData: number | null };

type FdEntry = {
  e: string;
  fd: number;
  kind: string;
  cloexec: boolean;
  status: number;
  offset: number | null;
  size: number;
  dev: number;
  ino: number;
  link: string;
};

// A descriptor that cannot be reopened at restore, pinned by valo with a
// private duplicate: guest fd number -> keepalive fd number.
type FdKeepalive = { fd: number; ka: number; dev: number; ino: number; link: string };

const PAGE_SIZE = 4096;
const PAGE_SIZE_LIMIT = 100;
const PAGE_FLAGS = ["No", "Zero", "Full", "File", "Guard"];

type Segment = {
  kind: "checkpoint" | "restore";
  slot: number;
  startUs?: number;
  endUs?: number;
  memoryStartUs?: number;
  memoryEndUs?: number;
  areas: Area[]; // emitted by the read loop (empty if it aborted earlier)
  savedDump: Area[]; // pre-merge dump of the snapshot's areas
  currentDump: Area[]; // pre-merge dump of /proc/self/maps at restore time
  fds: FdEntry[]; // open descriptors logged while the guest was frozen
  fdActions: { fd: number; action: string }[]; // what the fd sync did on restore
  keepalives: FdKeepalive[]; // pins for descriptors that cannot be reopened at restore
};

type Run = { pid: number; segments: Segment[] };

function parseFile(text: string): Run[] {
  const runs: Run[] = [];
  let run: Run | null = null;
  let cur: Segment | null = null;
  for (const line of text.split("\n")) {
    if (!line.trim()) continue;
    let ev: any;
    try {
      ev = JSON.parse(line);
    } catch {
      continue;
    }
    if (ev.e === "init") {
      run = { pid: ev.pid, segments: [] };
      runs.push(run);
      cur = null;
    } else if (run) {
      if (ev.e === "checkpoint.begin" || ev.e === "checkpoint_begin") {
        cur = {
          kind: "checkpoint",
          slot: ev.slot,
          startUs: ev.ts_us,
          areas: [],
          savedDump: [],
          currentDump: [],
          fds: [],
          fdActions: [],
          keepalives: [],
        };
        run.segments.push(cur);
      } else if (ev.e === "restore.begin" || ev.e === "restore_begin") {
        cur = {
          kind: "restore",
          slot: ev.slot,
          startUs: ev.ts_us,
          areas: [],
          savedDump: [],
          currentDump: [],
          fds: [],
          fdActions: [],
          keepalives: [],
        };
        run.segments.push(cur);
      } else if (cur) {
        if (ev.e === `${cur.kind}.fds`) cur.fds.push(ev);
        else if (cur.kind === "checkpoint" && ev.e === "checkpoint.fds.pin")
          cur.keepalives.push({ fd: ev.fd, ka: ev.ka, dev: ev.dev, ino: ev.ino, link: ev.link });
        else if (cur.kind === "restore" && ev.e.startsWith("restore.fds.")) {
          const action = ev.e.slice("restore.fds.".length);
          if (
            action === "close" ||
            action === "reopen" ||
            action === "reopen_failed" ||
            action === "missing"
          )
            cur.fdActions.push({ fd: ev.fd, action });
        } else if (ev.e === `${cur.kind}.end`) cur.endUs = ev.ts_us;
        else if (ev.e === `${cur.kind}.memory.begin`) cur.memoryStartUs = ev.ts_us;
        else if (ev.e === `${cur.kind}.memory.end`) cur.memoryEndUs = ev.ts_us;
        else if (
          ev.e === "save_area" ||
          ev.e === "restore_area" ||
          ev.e === "restore.memory.area.begin"
        )
          cur.areas.push(ev);
        else if (ev.e === "saved_area") cur.savedDump.push(ev);
        else if (ev.e === "current_area") cur.currentDump.push(ev);
        else if (ev.e === "save_pages" && cur.kind === "checkpoint") {
          const area = cur.areas.findLast((area) => area.start === ev.start);
          if (area) (area.pages ??= []).push({ index: ev.index, count: ev.count, flag: ev.flag });
        } else if (ev.e === "save_pagemap_pages" && cur.kind === "checkpoint") {
          const area = cur.areas.findLast((area) => area.start === ev.start);
          if (area) (area.pagemap ??= []).push({ index: ev.index, count: ev.count, bits: ev.bits });
        } else if (ev.e === "current_pagemap_pages" && cur.kind === "restore") {
          const area = cur.currentDump.findLast((area) => area.start === ev.start);
          if (area) (area.pagemap ??= []).push({ index: ev.index, count: ev.count, bits: ev.bits });
        } else if (ev.e === "current_pagemap_error" && cur.kind === "restore") {
          const area = cur.currentDump.findLast((area) => area.start === ev.start);
          if (area) area.pagemapError = true;
        } else if (ev.e === "shared_fd_initial") {
          const area = cur.areas.findLast((area) => area.start === ev.start);
          if (area)
            area.sharedFdInitial = {
              fd: ev.available ? ev.fd : null,
              nextHole: ev.available ? ev.next_hole : null,
              nextData: ev.available ? ev.next_data : null,
            };
        }
      }
    }
  }
  return runs;
}

function perfettoTrace(text: string): string {
  const traceEvents: object[] = [];
  let pid: number | null = null;
  for (const line of text.split("\n")) {
    if (!line.trim()) continue;
    let ev: any;
    try {
      ev = JSON.parse(line);
    } catch {
      continue;
    }
    if (ev.e === "init") {
      pid = ev.pid;
      continue;
    }
    if (pid === null || typeof ev.e !== "string" || !Number.isFinite(ev.ts_us)) continue;
    const separator = ev.e.lastIndexOf(".");
    if (separator < 1) continue;
    const phase = ev.e.slice(separator + 1);
    if (phase !== "begin" && phase !== "end") continue;
    traceEvents.push({
      name: ev.e.slice(0, separator),
      cat: "valo",
      ph: phase === "begin" ? "B" : "E",
      ts: ev.ts_us,
      pid,
      tid: 0,
      args: Object.fromEntries(
        Object.entries(ev).filter(([key]) => key !== "e" && key !== "ts_us"),
      ),
    });
  }
  return JSON.stringify({ traceEvents });
}

function openInPerfetto(text: string, fileName: string) {
  const origin = "https://ui.perfetto.dev";
  const perfettoWindow = window.open(origin, "_blank");
  if (!perfettoWindow) {
    window.alert("Allow popups for the visualizer to open Perfetto.");
    return;
  }

  const buffer = new TextEncoder().encode(perfettoTrace(text)).buffer;
  const onMessage = (event: MessageEvent) => {
    if (event.source !== perfettoWindow || event.origin !== origin || event.data !== "PONG") {
      return;
    }
    cleanup();
    perfettoWindow.postMessage(
      {
        perfetto: {
          buffer,
          title: fileName,
          fileName: fileName.replace(/\.jsonl$/, ".trace.json"),
        },
      },
      origin,
      [buffer],
    );
  };
  const ping = () => {
    if (perfettoWindow.closed) {
      cleanup();
      return;
    }
    perfettoWindow.postMessage("PING", origin);
  };
  const cleanup = () => {
    clearInterval(interval);
    clearTimeout(timeout);
    window.removeEventListener("message", onMessage);
  };
  window.addEventListener("message", onMessage);
  const interval = setInterval(ping, 100);
  const timeout = setTimeout(cleanup, 20_000);
  ping();
}

type DiffRow = {
  snapshot: Area | null;
  process: Area | null;
  restored?: Area;
  ok: boolean;
};

function diffAreas(left: Area[], right: Area[]): DiffRow[] {
  const byStart = new Map<number, Area>();
  for (const a of right) byStart.set(a.start, a);

  const rows: DiffRow[] = [];
  for (const l of left) {
    const r = byStart.get(l.start);
    rows.push({
      snapshot: l,
      process: r ?? null,
      ok: r !== undefined && r.end === l.end && r.perms === l.perms && r.path === l.path,
    });
    if (r) byStart.delete(r.start);
  }
  for (const r of byStart.values()) {
    rows.push({ snapshot: null, process: r, ok: false });
  }
  rows.sort(
    (a, b) => (a.snapshot?.start ?? a.process!.start) - (b.snapshot?.start ?? b.process!.start),
  );
  return rows;
}

function hex(n: number | null): string {
  return n === null ? "—" : "0x" + n.toString(16);
}

function fmtSize(bytes: number): string {
  if (bytes >= 1 << 30) return (bytes / (1 << 30)).toFixed(1) + " GiB";
  if (bytes >= 1 << 20) return (bytes / (1 << 20)).toFixed(1) + " MiB";
  if (bytes >= 1 << 10) return (bytes / (1 << 10)).toFixed(1) + " KiB";
  return bytes + " B";
}

function escapeHtml(value: string): string {
  return value.replace(
    /[&<>"']/g,
    (char) =>
      ({
        "&": "&amp;",
        "<": "&lt;",
        ">": "&gt;",
        '"': "&quot;",
        "'": "&#39;",
      })[char]!,
  );
}

function diffValue<T>(
  before: T | null,
  after: T | null,
  format: (value: T | null) => string,
): string {
  const oldValue = format(before);
  const newValue = format(after);
  if (before === null) return newValue;
  if (after === null) return oldValue;
  return before === after ? oldValue : `<span class="mismatch">${oldValue} → ${newValue}</span>`;
}

function textValue(value: string | null): string {
  return value === null ? "—" : escapeHtml(value);
}

function changedFields(row: DiffRow): ("start" | "end" | "perms" | "path")[] {
  return (["start", "end", "perms", "path"] as const).filter(
    (field) => row.snapshot?.[field] !== row.process?.[field],
  );
}

function changedFieldValue(row: DiffRow, field: "start" | "end" | "perms" | "path"): string {
  if (field === "start" || field === "end")
    return diffValue(row.snapshot?.[field] ?? null, row.process?.[field] ?? null, hex);
  return diffValue(row.snapshot?.[field] ?? null, row.process?.[field] ?? null, textValue);
}

type FdDiffRow = { checkpoint: FdEntry | null; process: FdEntry | null; ok: boolean };

function fdEqual(a: FdEntry, b: FdEntry): boolean {
  return (
    a.kind === b.kind &&
    a.cloexec === b.cloexec &&
    a.status === b.status &&
    a.offset === b.offset &&
    a.size === b.size &&
    a.dev === b.dev &&
    a.ino === b.ino &&
    a.link === b.link
  );
}

function diffFds(left: FdEntry[], right: FdEntry[]): FdDiffRow[] {
  const byFd = new Map(right.map((entry) => [entry.fd, entry]));
  const rows: FdDiffRow[] = [];
  for (const l of left) {
    const r = byFd.get(l.fd) ?? null;
    byFd.delete(l.fd);
    rows.push({ checkpoint: l, process: r, ok: r !== null && fdEqual(l, r) });
  }
  for (const r of byFd.values()) rows.push({ checkpoint: null, process: r, ok: false });
  rows.sort((a, b) => (a.checkpoint?.fd ?? a.process!.fd) - (b.checkpoint?.fd ?? b.process!.fd));
  return rows;
}

const fdNum = (n: number | null) => (n === null ? "—" : n.toLocaleString());
const fdHex = (n: number | null) => (n === null ? "—" : "0x" + n.toString(16));
const fdBool = (b: boolean | null) => (b === null ? "—" : String(b));
const fdIdentity = (entry: FdEntry | null) => (entry === null ? null : `${entry.dev}:${entry.ino}`);

function fdRowCells(checkpoint: FdEntry | null, process: FdEntry | null): string {
  return `<td>${diffValue(checkpoint?.kind ?? null, process?.kind ?? null, textValue)}</td>
    <td>${diffValue(checkpoint?.cloexec ?? null, process?.cloexec ?? null, fdBool)}</td>
    <td>${diffValue(checkpoint?.status ?? null, process?.status ?? null, fdHex)}</td>
    <td>${diffValue(checkpoint?.offset ?? null, process?.offset ?? null, fdNum)}</td>
    <td>${diffValue(checkpoint?.size ?? null, process?.size ?? null, (n) => (n === null ? "—" : fmtSize(n)))}</td>
    <td>${diffValue(fdIdentity(checkpoint), fdIdentity(process), textValue)}</td>
    <td>${diffValue(checkpoint?.link ?? null, process?.link ?? null, textValue)}</td>`;
}

const FD_COLUMNS =
  "<tr><th>fd</th><th>kind</th><th>cloexec</th><th>status</th><th>offset</th><th>size</th><th>dev:ino</th><th>link</th></tr>";

function fdTable(entries: FdEntry[]): string {
  return `<table>${FD_COLUMNS}
    ${entries.map((entry) => `<tr><td>${entry.fd}</td>${fdRowCells(entry, entry)}</tr>`).join("")}
  </table>`;
}

function fdDiffTable(rows: FdDiffRow[]): string {
  return `<table>${FD_COLUMNS}
    ${rows
      .map((row) => {
        const status = !row.checkpoint ? "added" : !row.process ? "removed" : "";
        return `<tr class="${row.ok ? "" : "bad"}"><td>${row.checkpoint?.fd ?? row.process!.fd}${status ? ` <span class="mismatch">${status}</span>` : ""}</td>${fdRowCells(row.checkpoint, row.process)}</tr>`;
      })
      .join("")}
  </table>`;
}

function fdSection(title: string, table: string): string {
  return `<details class="fd-list"><summary>${title}</summary>${table}</details>`;
}

function keepaliveTable(entries: FdKeepalive[]): string {
  return `<table>
    <tr><th>fd</th><th>keepalive</th><th>dev:ino</th><th>link</th></tr>
    ${entries.map((k) => `<tr><td>${k.fd}</td><td>${k.ka}</td><td>${k.dev}:${k.ino}</td><td>${k.link}</td></tr>`).join("")}
  </table>`;
}

function fdRowSummary(actions: { fd: number; action: string }[]): string {
  const counts = new Map<string, number>();
  for (const { action } of actions) counts.set(action, (counts.get(action) ?? 0) + 1);
  return [...counts]
    .sort()
    .map(([action, count]) => `${count} ${action}`)
    .join(", ");
}

function runAt<T extends { index: number; count: number }>(
  runs: T[] | undefined,
  index: number,
): T | undefined {
  return runs?.find((run) => run.index <= index && index < run.index + run.count);
}

const PAGEMAP_BITS = ["present", "file/shared-anon", "swapped", "guard", "soft-dirty", "exclusive"];

function formatPagemap(bits: number | null): string {
  if (bits === null) return "—";
  const flags = PAGEMAP_BITS.filter((_, bit) => bits & (1 << bit));
  return flags.length ? flags.join(", ") : "none";
}

function formatSharedFd(info: SharedFdInitial | undefined): string {
  if (!info) return "—";
  if (info.fd === null) return "unavailable";
  return `fd ${info.fd}, next hole offset ${hex(info.nextHole)}, next data offset ${hex(info.nextData)}`;
}

function pageDetails(area: Area, page: number, diff?: DiffRow): string {
  const count = Math.ceil((area.end - area.start) / PAGE_SIZE);
  const pageCount = Math.ceil(count / PAGE_SIZE_LIMIT);
  const runs = area.pages ?? [];
  const counts = PAGE_FLAGS.map((_, flag) =>
    runs.filter((run) => run.flag === flag).reduce((sum, run) => sum + run.count, 0),
  );
  const summary = runs.length
    ? PAGE_FLAGS.map((flag, i) => `${flag}: ${counts[i]}`).join(" · ")
    : area.skip
      ? "Skipped: no page flags saved"
      : "No page flags in log";
  const first = page * PAGE_SIZE_LIMIT;
  const last = Math.min(first + PAGE_SIZE_LIMIT, count);
  const rows: string[] = [];
  for (let index = first; index < last; index++) {
    const flag = runAt(runs, index);
    const savedBits = runAt(area.pagemap, index)?.bits ?? null;
    const currentBits = diff?.process?.pagemapError
      ? null
      : (runAt(diff?.process?.pagemap, index)?.bits ?? null);
    const pagemap = diff
      ? diffValue(savedBits, currentBits, formatPagemap)
      : formatPagemap(savedBits);
    rows.push(
      `<tr><td>${index}</td><td>${hex(area.start + index * PAGE_SIZE)}</td><td>${flag ? (PAGE_FLAGS[flag.flag] ?? "Unknown") : "—"}</td><td>${pagemap}</td></tr>`,
    );
  }
  const comparison = diff
    ? `<p>${
        changedFields(diff)
          .map((field) => `${field}: ${changedFieldValue(diff, field)}`)
          .join(" · ") || "Mapping unchanged"
      }</p>
       ${diff.process?.pagemapError ? '<p class="mismatch">Process pagemap could not be read completely.</p>' : ""}`
    : "";
  const sharedFd =
    area.sharedFdInitial || diff?.restored?.sharedFdInitial
      ? `<p class="dim">Shared fd at area start · checkpoint: ${formatSharedFd(area.sharedFdInitial)}${diff ? ` · restore: ${formatSharedFd(diff.restored?.sharedFdInitial)}` : ""}</p>`
      : "";
  return `${comparison}${sharedFd}<p class="dim">${count} Pages · ${diff && !diff.snapshot ? "no snapshot area" : `Snapshot page flags · ${summary}`}</p>
    <table><tr><th>#</th><th>address</th><th>Snapshot flag</th><th>Pagemap</th></tr>${rows.join("")}</table>
    ${
      pageCount > 1
        ? `<div class="page-nav"><button type="button" data-page="${page - 1}" ${page === 0 ? "disabled" : ""}>←</button>
      ${page + 1} / ${pageCount}
      <button type="button" data-page="${page + 1}" ${page + 1 >= pageCount ? "disabled" : ""}>→</button></div>`
        : ""
    }`;
}

function detailRow(area: Area, index: number, diff = false): string {
  return `<tr class="area-detail"><td colspan="${diff ? 4 : 5}"><details id="area-pages-${index}" data-area="${index}">
    <summary>${diff ? "Mapping comparison and snapshot pages" : "Page details"} · ${hex(area.start)}–${hex(area.end)}</summary>
    <div class="page-content"></div>
  </details></td></tr>`;
}

function areaTable(areas: Area[]): string {
  const maxSpan = Math.max(...areas.map((a) => a.end - a.start), 1);
  return `<table>
    <tr><th>start</th><th>perms</th><th colspan="2">size</th><th>path</th></tr>
    ${areas
      .map((a, index) => {
        const span = a.end - a.start;
        const width = Math.max(2, Math.round((Math.log2(span) / Math.log2(maxSpan)) * 90));
        return `<tr class="area-row ${a.skip ? "skip" : ""}" data-area="${index}">
          <td><button type="button" class="area-toggle" data-area="${index}" aria-expanded="false" aria-controls="area-pages-${index}" aria-label="Page details for area ${hex(a.start)}">${hex(a.start)}</button></td>
          <td>${a.perms}</td>
          <td><span class="bar ${a.skip ? "skip" : ""}" title="${fmtSize(span)}" style="width:${width}px"></span></td>
          <td class="size">${fmtSize(span)}</td>
          <td>${escapeHtml(a.path || "")}</td>
        </tr>${detailRow(a, index)}`;
      })
      .join("")}
  </table>`;
}

function diffTable(rows: DiffRow[], areas: Area[]): string {
  return `<table>
    <tr><th>start</th><th>end</th><th>perms</th><th>path</th></tr>
    ${rows
      .map((r, index) => {
        const cls = `${r.ok ? "" : "bad"} ${r.snapshot?.skip || areas[index].skip ? "skip" : ""}`;
        const status = !r.snapshot ? "added" : !r.process ? "removed" : "";
        const start = r.snapshot?.start ?? r.process!.start;
        return `<tr class="area-row ${cls}" data-area="${index}">
          <td><button type="button" class="area-toggle" data-area="${index}" aria-expanded="false" aria-controls="area-pages-${index}" aria-label="Page details for area ${hex(start)}">${diffValue(r.snapshot?.start ?? null, r.process?.start ?? null, hex)}</button>${status ? ` <span class="mismatch">${status}</span>` : ""}</td>
          <td>${diffValue(r.snapshot?.end ?? null, r.process?.end ?? null, hex)}</td>
          <td>${diffValue(r.snapshot?.perms ?? null, r.process?.perms ?? null, textValue)}</td>
          <td>${diffValue(r.snapshot?.path ?? null, r.process?.path ?? null, textValue)}</td>
        </tr>${detailRow(areas[index], index, true)}`;
      })
      .join("")}
  </table>`;
}

function globalStats(
  seg: Segment,
  changed?: number,
  checkpoint?: Segment,
  rows?: DiffRow[],
): string {
  const duration = (start?: number, end?: number) =>
    start === undefined || end === undefined ? "—" : `${((end - start) / 1000).toFixed(1)} ms`;
  const row = (label: string, value: string | number) =>
    `<tr><th>${label}</th><td>${typeof value === "number" ? value.toLocaleString() : value}</td></tr>`;
  const stats = [
    row("Total time", duration(seg.startUs, seg.endUs)),
    row("Memory time", duration(seg.memoryStartUs, seg.memoryEndUs)),
  ];

  if (seg.kind === "checkpoint") {
    const counts = PAGE_FLAGS.map((_, flag) =>
      seg.areas.reduce(
        (sum, area) =>
          sum + (area.pages?.reduce((n, run) => n + (run.flag === flag ? run.count : 0), 0) ?? 0),
        0,
      ),
    );
    stats.push(
      row(
        "Areas / skipped",
        `${seg.areas.length.toLocaleString()} / ${seg.areas.filter((a) => a.skip).length.toLocaleString()}`,
      ),
      row("Mapped size", fmtSize(seg.areas.reduce((sum, a) => sum + a.end - a.start, 0))),
      ...PAGE_FLAGS.map((flag, i) => row(`${flag} pages`, counts[i])),
      row("Full page data", fmtSize(counts[2] * PAGE_SIZE)),
    );
  } else {
    const runs = seg.currentDump.flatMap((area) => area.pagemap ?? []);
    const scanned = runs.reduce((sum, run) => sum + run.count, 0);
    const dirty = runs.reduce((sum, run) => sum + (run.bits & (1 << 4) ? run.count : 0), 0);
    const full = checkpoint && rows && fullPageStats(checkpoint, rows);
    stats.push(
      row(
        "Snapshot / current areas",
        `${seg.savedDump.length.toLocaleString()} / ${seg.currentDump.length.toLocaleString()}`,
      ),
      row("Changed areas", changed ?? 0),
      row("Areas restored", seg.areas.length),
      row("Current pages scanned", scanned),
      row(
        "Current soft-dirty pages",
        `${dirty.toLocaleString()} / ${scanned.toLocaleString()}${scanned ? ` (${((dirty / scanned) * 100).toFixed(1)}%)` : ""}`,
      ),
      row("Pagemap read errors", seg.currentDump.filter((area) => area.pagemapError).length),
    );
    if (full) {
      const value = (count: number) =>
        `${count.toLocaleString()} / ${full.total.toLocaleString()} (${full.total ? ((count / full.total) * 100).toFixed(1) : "0.0"}%; ${fmtSize(count * PAGE_SIZE)})`;
      stats.push(
        row(
          "Snapshot Full pages",
          `${full.total.toLocaleString()} (${fmtSize(full.total * PAGE_SIZE)})`,
        ),
        row("Full pages clean + present in matching private areas", value(full.cleanPresent)),
        row("Full pages soft-dirty in matching private areas", value(full.dirty)),
        row("Full pages unclassified", value(full.total - full.cleanPresent - full.dirty)),
      );
      if (full.changed) stats.push(row("↳ Changed or missing mapping", value(full.changed)));
      if (full.shared) stats.push(row("↳ Shared mapping", value(full.shared)));
      if (full.unavailable) stats.push(row("↳ Pagemap unavailable", value(full.unavailable)));
      if (full.notPresent) stats.push(row("↳ Clean but not present", value(full.notPresent)));
    }
  }
  const note =
    seg.kind === "restore" && checkpoint
      ? '<p class="dim">Full pages are classified only when the private mapping still matches and its pagemap was read completely. Unclassified also includes pages that are no longer present.</p>'
      : "";
  return `<section class="global-stats"><h3>Global stats</h3><table>${stats.join("")}</table>${note}</section>`;
}

function fullPageStats(checkpoint: Segment, rows: DiffRow[]) {
  const byStart = new Map(
    rows.filter((row) => row.snapshot).map((row) => [row.snapshot!.start, row]),
  );
  let total = 0;
  let cleanPresent = 0;
  let dirty = 0;
  let changed = 0;
  let shared = 0;
  let unavailable = 0;
  let notPresent = 0;

  for (const area of checkpoint.areas) {
    const pages = area.pages ?? [];
    const full = pages.reduce((n, run) => n + (run.flag === 2 ? run.count : 0), 0);
    total += full;
    if (!full) continue;

    const row = byStart.get(area.start);
    const saved = row?.snapshot;
    const current = row?.process;
    if (
      !row?.ok ||
      !saved ||
      area.end !== saved.end ||
      area.perms !== saved.perms ||
      area.path !== saved.path
    ) {
      changed += full;
      continue;
    }
    if (!saved.perms.endsWith("p")) {
      shared += full;
      continue;
    }
    if (!current?.pagemap || current.pagemapError) {
      unavailable += full;
      continue;
    }

    let i = 0;
    let j = 0;
    let classified = 0;
    while (i < pages.length && j < current.pagemap.length) {
      const flag = pages[i];
      const map = current.pagemap[j];
      const overlap =
        Math.min(flag.index + flag.count, map.index + map.count) - Math.max(flag.index, map.index);
      if (flag.flag === 2 && overlap > 0) {
        if (map.bits & (1 << 4)) dirty += overlap;
        else if (map.bits & 1) cleanPresent += overlap;
        else notPresent += overlap;
        classified += overlap;
      }
      if (flag.index + flag.count <= map.index + map.count) i++;
      if (map.index + map.count <= flag.index + flag.count) j++;
    }
    unavailable += full - classified;
  }
  return { total, cleanPresent, dirty, changed, shared, unavailable, notPresent };
}

async function main() {
  const sel = document.getElementById("file") as HTMLSelectElement;
  const del = document.getElementById("delete") as HTMLButtonElement;
  const openPerfetto = document.getElementById("open-perfetto") as HTMLButtonElement;
  const perfetto = document.getElementById("perfetto") as HTMLButtonElement;
  const treeEl = document.getElementById("tree") as HTMLDivElement;
  const viewEl = document.getElementById("view") as HTMLDivElement;

  let runs: Run[] = [];
  let loadedText = "";
  let visibleAreas: Area[] = [];
  let visibleDiffRows: DiffRow[] = [];
  openPerfetto.disabled = true;
  perfetto.disabled = true;

  function renderTree() {
    treeEl.innerHTML = "";
    for (const [ri, run] of runs.entries()) {
      const runDiv = document.createElement("div");
      runDiv.className = "run";
      runDiv.innerHTML = `<div class="run-head">pid ${run.pid}</div>`;

      for (const [si, seg] of run.segments.entries()) {
        const aborted = seg.kind === "restore" && seg.areas.length === 0;
        const icon = seg.kind === "checkpoint" ? "💾" : aborted ? "⚠" : "↩";
        const extra =
          seg.kind === "restore" && aborted ? " <span class='mismatch'>aborted</span>" : "";
        const div = document.createElement("button");
        div.type = "button";
        div.className = `seg ${seg.kind}${aborted ? " aborted" : ""}`;
        div.dataset.run = String(ri);
        div.dataset.seg = String(si);
        div.innerHTML = `${icon} ${seg.kind} slot ${seg.slot} <span class="dim">${seg.areas.length} areas${extra}</span>`;
        div.addEventListener("click", () => showSegment(ri, si));
        runDiv.appendChild(div);
      }
      treeEl.appendChild(runDiv);
    }
  }

  function showSegment(ri: number, si: number) {
    const run = runs[ri];
    const seg = run.segments[si];
    for (const div of treeEl.querySelectorAll<HTMLElement>(".seg")) {
      div.classList.toggle(
        "active",
        Number(div.dataset.run) === ri && Number(div.dataset.seg) === si,
      );
    }

    if (seg.kind === "checkpoint") {
      visibleAreas = seg.areas;
      visibleDiffRows = [];
      const kaView = seg.keepalives.length
        ? fdSection(
            `keepalives — ${seg.keepalives.length} pinned <span class="dim">(descriptors that cannot be reopened at restore)</span>`,
            keepaliveTable(seg.keepalives),
          )
        : "";
      viewEl.innerHTML = `<h2>💾 checkpoint slot ${seg.slot} — ${seg.areas.length} areas</h2>${areaTable(seg.areas)}${fdSection(`file descriptors — ${seg.fds.length}`, fdTable(seg.fds))}${kaView}${globalStats(seg)}`;
      return;
    }

    // Restore: the interesting diff is the pre-merge dump
    // (snapshot areas vs /proc/self/maps at restore time) — it shows the
    // mismatch that killed the restore even if the read loop never ran.
    const rows = diffAreas(seg.savedDump, seg.currentDump);
    for (const row of rows) {
      row.restored = seg.areas.find((area) => area.start === row.snapshot?.start);
    }
    const checkpoint = run.segments
      .slice(0, si)
      .reverse()
      .find((candidate) => candidate.kind === "checkpoint" && candidate.slot === seg.slot);
    visibleDiffRows = rows;
    visibleAreas = rows.map(
      (row) =>
        (row.snapshot && checkpoint?.areas.find((area) => area.start === row.snapshot?.start)) ??
        row.snapshot ??
        row.process!,
    );
    const changed = rows.filter((r) => !r.ok).length;
    const fdRows = diffFds(checkpoint?.fds ?? [], seg.fds);
    const fdChanged = fdRows.filter((r) => !r.ok).length;
    const missing = seg.fdActions.filter((a) => a.action === "missing");
    const fdReconcile = seg.fdActions.length
      ? `<p class="dim">fd sync: ${fdRowSummary(seg.fdActions)}${missing.length ? ` · missing: ${missing.map((a) => a.fd).join(", ")}` : ""}</p>`
      : "";
    const kaNote = checkpoint?.keepalives.length
      ? `<p class="dim">${checkpoint.keepalives.length} keepalives pin the descriptors that cannot be reopened (restore re-dups them onto their fd numbers)</p>`
      : "";
    const fdView = checkpoint
      ? fdSection(
          `file descriptors — checkpoint ${checkpoint.fds.length} vs process ${seg.fds.length} <span class="${fdChanged ? "mismatch" : ""}">${fdChanged} changed</span>`,
          `${fdDiffTable(fdRows)}${fdReconcile}${kaNote}`,
        )
      : fdSection(
          `file descriptors — ${seg.fds.length} (no matching checkpoint)`,
          fdTable(seg.fds),
        );
    const readNote =
      seg.areas.length > 0
        ? `<p class="dim">${seg.areas.length} areas read after reallocation</p>`
        : `<p class="mismatch">aborted during reallocation — no areas were read</p>`;

    viewEl.innerHTML = `<h2>↩ restore slot ${seg.slot} — snapshot vs process <span class="${changed ? "mismatch" : ""}">${changed} changed</span></h2>
      ${readNote}
      ${diffTable(rows, visibleAreas)}
      ${fdView}
      ${globalStats(seg, changed, checkpoint, rows)}`;
  }

  viewEl.addEventListener("mousedown", (event) => {
    if (
      event.detail > 1 &&
      (event.target as HTMLElement).closest("tr.area-row[data-area], .area-detail summary")
    ) {
      event.preventDefault();
    }
  });

  viewEl.addEventListener("click", (event) => {
    const renderDetails = (details: HTMLDetailsElement) => {
      const area = visibleAreas[Number(details.dataset.area)];
      const content = details.querySelector<HTMLDivElement>(".page-content");
      if (area && content && !content.hasChildNodes())
        content.innerHTML = pageDetails(area, 0, visibleDiffRows[Number(details.dataset.area)]);
    };
    const button = (event.target as HTMLElement).closest<HTMLButtonElement>("button[data-page]");
    if (button) {
      const details = button.closest<HTMLDetailsElement>("details[data-area]");
      const area = details && visibleAreas[Number(details.dataset.area)];
      const page = Number(button.dataset.page);
      if (!area || !Number.isInteger(page) || page < 0) return;
      const content = details.querySelector<HTMLDivElement>(".page-content");
      if (content)
        content.innerHTML = pageDetails(area, page, visibleDiffRows[Number(details.dataset.area)]);
      return;
    }
    const summary = (event.target as HTMLElement).closest("details[data-area] > summary");
    if (summary) {
      renderDetails(summary.parentElement as HTMLDetailsElement);
      return;
    }
    const row = (event.target as HTMLElement).closest<HTMLTableRowElement>(
      "tr.area-row[data-area]",
    );
    if (row) {
      const details = row.nextElementSibling?.querySelector<HTMLDetailsElement>("details");
      if (details) {
        if (!details.open) renderDetails(details);
        details.open = !details.open;
      }
    }
  });
  viewEl.addEventListener(
    "toggle",
    (event) => {
      const details = event.target;
      if (!(details instanceof HTMLDetailsElement) || !details.matches("details[data-area]"))
        return;
      const button = details
        .closest("tr")
        ?.previousElementSibling?.querySelector<HTMLButtonElement>(".area-toggle");
      button?.setAttribute("aria-expanded", String(details.open));
    },
    true,
  );

  async function refreshFiles() {
    sel.innerHTML = "";
    const files = await (await fetch("/api/files")).json();
    for (const f of files) {
      const opt = document.createElement("option");
      opt.value = f.name;
      opt.textContent = f.name;
      sel.appendChild(opt);
    }
    return files;
  }

  async function load(name: string) {
    openPerfetto.disabled = true;
    perfetto.disabled = true;
    const text = await (await fetch(`/api/file/${name}`)).text();
    loadedText = text;
    runs = parseFile(text);
    visibleAreas = [];
    visibleDiffRows = [];
    renderTree();
    viewEl.innerHTML = "<p class='dim'>select a checkpoint or restore</p>";
    openPerfetto.disabled = false;
    perfetto.disabled = false;
  }

  sel.onchange = () => load(sel.value);
  openPerfetto.onclick = () => openInPerfetto(loadedText, sel.value);
  perfetto.onclick = () => {
    const blob = new Blob([perfettoTrace(loadedText)], { type: "application/json" });
    const url = URL.createObjectURL(blob);
    const link = document.createElement("a");
    link.href = url;
    link.download = sel.value.replace(/\.jsonl$/, ".trace.json");
    link.click();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  };
  del.onclick = async () => {
    del.disabled = true;
    try {
      await fetch("/api/delete-all", { method: "POST" });
      runs = [];
      visibleAreas = [];
      visibleDiffRows = [];
      loadedText = "";
      openPerfetto.disabled = true;
      perfetto.disabled = true;
      treeEl.innerHTML = "";
      viewEl.innerHTML = "";
      await refreshFiles();
    } finally {
      del.disabled = false;
    }
  };

  const files = await refreshFiles();
  if (files.length > 0) load(files[0].name);
}

main();

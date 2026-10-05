type Area = {
  e: string;
  start: number;
  end: number;
  perms: string;
  path: string;
  skip?: boolean;
  pages?: PageRun[];
};

type PageRun = { index: number; count: number; flag: number };

const PAGE_SIZE = 4096;
const PAGE_SIZE_LIMIT = 100;
const PAGE_FLAGS = ["No", "Zero", "Full", "File", "Guard"];

type Segment = {
  kind: "checkpoint" | "restore";
  slot: number;
  areas: Area[]; // emitted by the read loop (empty if it aborted earlier)
  savedDump: Area[]; // pre-merge dump of the snapshot's areas
  currentDump: Area[]; // pre-merge dump of /proc/self/maps at restore time
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
        cur = { kind: "checkpoint", slot: ev.slot, areas: [], savedDump: [], currentDump: [] };
        run.segments.push(cur);
      } else if (ev.e === "restore.begin" || ev.e === "restore_begin") {
        cur = { kind: "restore", slot: ev.slot, areas: [], savedDump: [], currentDump: [] };
        run.segments.push(cur);
      } else if (cur) {
        if (ev.e === "save_area" || ev.e === "restore_area" || ev.e === "restore.memory.area.begin")
          cur.areas.push(ev);
        else if (ev.e === "saved_area") cur.savedDump.push(ev);
        else if (ev.e === "current_area") cur.currentDump.push(ev);
        else if (ev.e === "save_pages" && cur.kind === "checkpoint") {
          const area = cur.areas.findLast((area) => area.start === ev.start);
          if (area) (area.pages ??= []).push({ index: ev.index, count: ev.count, flag: ev.flag });
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
  start: number;
  leftEnd: number | null;
  rightEnd: number | null;
  perms: string;
  path: string;
  ok: boolean;
};

function diffAreas(left: Area[], right: Area[]): DiffRow[] {
  const byStart = new Map<number, Area>();
  for (const a of right) byStart.set(a.start, a);

  const rows: DiffRow[] = [];
  for (const l of left) {
    const r = byStart.get(l.start);
    const ok = r !== undefined && r.end === l.end && r.perms === l.perms;
    rows.push({
      start: l.start,
      leftEnd: l.end,
      rightEnd: r ? r.end : null,
      perms: l.perms,
      path: l.path,
      ok,
    });
    if (r) byStart.delete(r.start);
  }
  for (const r of byStart.values()) {
    rows.push({
      start: r.start,
      leftEnd: null,
      rightEnd: r.end,
      perms: r.perms,
      path: r.path,
      ok: false,
    });
  }
  rows.sort((a, b) => a.start - b.start);
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

function pageDetails(area: Area, page: number): string {
  const count = Math.ceil((area.end - area.start) / PAGE_SIZE);
  const pageCount = Math.ceil(count / PAGE_SIZE_LIMIT);
  const runs = area.pages ?? [];
  const counts = PAGE_FLAGS.map((_, flag) =>
    runs.filter((run) => run.flag === flag).reduce((sum, run) => sum + run.count, 0),
  );
  const summary = runs.length
    ? PAGE_FLAGS.map((flag, i) => `${flag}: ${counts[i]}`).join(" · ")
    : area.skip
      ? "Übersprungen: keine Page-Flags gespeichert"
      : "Keine Page-Flags im Log vorhanden";
  const first = page * PAGE_SIZE_LIMIT;
  const last = Math.min(first + PAGE_SIZE_LIMIT, count);
  let runIndex = 0;
  const rows: string[] = [];
  for (let index = first; index < last; index++) {
    while (runIndex < runs.length && runs[runIndex].index + runs[runIndex].count <= index)
      runIndex++;
    const run = runs[runIndex];
    const flag = run && run.index <= index ? (PAGE_FLAGS[run.flag] ?? "Unknown") : "—";
    rows.push(
      `<tr><td>${index}</td><td>${hex(area.start + index * PAGE_SIZE)}</td><td>${flag}</td></tr>`,
    );
  }
  return `<p class="dim">${count} Pages · ${summary}</p>
    <table><tr><th>#</th><th>address</th><th>flag</th></tr>${rows.join("")}</table>
    ${
      pageCount > 1
        ? `<div class="page-nav"><button type="button" data-page="${page - 1}" ${page === 0 ? "disabled" : ""}>←</button>
      ${page + 1} / ${pageCount}
      <button type="button" data-page="${page + 1}" ${page + 1 >= pageCount ? "disabled" : ""}>→</button></div>`
        : ""
    }`;
}

function detailRow(area: Area, index: number): string {
  return `<tr class="area-detail"><td colspan="5"><details id="area-pages-${index}" data-area="${index}">
    <summary>Page details · ${hex(area.start)}–${hex(area.end)}</summary>
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

function diffTable(rows: DiffRow[], leftName: string, rightName: string, areas: Area[]): string {
  return `<table>
    <tr><th>start</th><th>perms</th><th>${leftName} end</th><th>${rightName} end</th><th>path</th></tr>
    ${rows
      .map((r, index) => {
        const cls = r.ok ? "" : "bad";
        return `<tr class="area-row ${cls}" data-area="${index}">
          <td><button type="button" class="area-toggle" data-area="${index}" aria-expanded="false" aria-controls="area-pages-${index}" aria-label="Page details for area ${hex(r.start)}">${hex(r.start)}</button></td>
          <td>${r.perms}</td>
          <td>${hex(r.leftEnd)}</td>
          <td>${hex(r.rightEnd)}</td>
          <td>${escapeHtml(r.path || "")}${r.ok ? "" : " <span class='mismatch'>⟵ changed</span>"}</td>
        </tr>${detailRow(areas[index], index)}`;
      })
      .join("")}
  </table>`;
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
      viewEl.innerHTML = `<h2>💾 checkpoint slot ${seg.slot} — ${seg.areas.length} areas</h2>${areaTable(seg.areas)}`;
      return;
    }

    // Restore: the interesting diff is the pre-merge dump
    // (snapshot areas vs /proc/self/maps at restore time) — it shows the
    // mismatch that killed the restore even if the read loop never ran.
    const rows = diffAreas(seg.savedDump, seg.currentDump);
    const checkpoint = run.segments
      .slice(0, si)
      .reverse()
      .find((candidate) => candidate.kind === "checkpoint" && candidate.slot === seg.slot);
    visibleAreas = rows.map(
      (row) =>
        checkpoint?.areas.find((area) => area.start === row.start) ?? {
          e: "area",
          start: row.start,
          end: row.leftEnd ?? row.rightEnd ?? row.start,
          perms: row.perms,
          path: row.path,
        },
    );
    const changed = rows.filter((r) => !r.ok).length;
    const readNote =
      seg.areas.length > 0
        ? `<p class="dim">${seg.areas.length} areas read after reallocation</p>`
        : `<p class="mismatch">aborted during reallocation — no areas were read</p>`;

    viewEl.innerHTML = `<h2>↩ restore slot ${seg.slot} — snapshot vs process <span class="${changed ? "mismatch" : ""}">${changed} changed</span></h2>
      ${readNote}
      ${diffTable(rows, "snapshot", "process", visibleAreas)}`;
  }

  viewEl.addEventListener("click", (event) => {
    const renderDetails = (details: HTMLDetailsElement) => {
      const area = visibleAreas[Number(details.dataset.area)];
      const content = details.querySelector<HTMLDivElement>(".page-content");
      if (area && content && !content.hasChildNodes()) content.innerHTML = pageDetails(area, 0);
    };
    const button = (event.target as HTMLElement).closest<HTMLButtonElement>("button[data-page]");
    if (button) {
      const details = button.closest<HTMLDetailsElement>("details[data-area]");
      const area = details && visibleAreas[Number(details.dataset.area)];
      const page = Number(button.dataset.page);
      if (!area || !Number.isInteger(page) || page < 0) return;
      const content = details.querySelector<HTMLDivElement>(".page-content");
      if (content) content.innerHTML = pageDetails(area, page);
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

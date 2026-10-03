type Area = {
  e: string;
  start: number;
  end: number;
  perms: string;
  path: string;
  skip?: boolean;
};

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
      if (ev.e === "checkpoint_begin") {
        cur = { kind: "checkpoint", slot: ev.slot, areas: [], savedDump: [], currentDump: [] };
        run.segments.push(cur);
      } else if (ev.e === "restore_begin") {
        cur = { kind: "restore", slot: ev.slot, areas: [], savedDump: [], currentDump: [] };
        run.segments.push(cur);
      } else if (cur) {
        if (ev.e === "save_area" || ev.e === "restore_area") cur.areas.push(ev);
        else if (ev.e === "saved_area") cur.savedDump.push(ev);
        else if (ev.e === "current_area") cur.currentDump.push(ev);
      }
    }
  }
  return runs;
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

function areaTable(areas: Area[]): string {
  const maxSpan = Math.max(...areas.map((a) => a.end - a.start), 1);
  return `<table>
    <tr><th>start</th><th>perms</th><th>size</th><th>path</th></tr>
    ${areas
      .map((a) => {
        const span = a.end - a.start;
        const width = Math.max(2, Math.round((Math.log2(span) / Math.log2(maxSpan)) * 90));
        return `<tr class="${a.skip ? "skip" : ""}">
          <td>${hex(a.start)}</td>
          <td>${a.perms}</td>
          <td><span class="bar ${a.skip ? "skip" : ""}" title="${fmtSize(span)}" style="width:${width}px"></span></td>
          <td class="size">${fmtSize(span)}</td>
          <td>${a.path || ""}</td>
        </tr>`;
      })
      .join("")}
  </table>`;
}

function diffTable(rows: DiffRow[], leftName: string, rightName: string): string {
  return `<table>
    <tr><th>start</th><th>perms</th><th>${leftName} end</th><th>${rightName} end</th><th>path</th></tr>
    ${rows
      .map((r) => {
        const cls = r.ok ? "" : "bad";
        return `<tr class="${cls}">
          <td>${hex(r.start)}</td>
          <td>${r.perms}</td>
          <td>${hex(r.leftEnd)}</td>
          <td>${hex(r.rightEnd)}</td>
          <td>${r.path || ""}${r.ok ? "" : " <span class='mismatch'>⟵ changed</span>"}</td>
        </tr>`;
      })
      .join("")}
  </table>`;
}

async function main() {
  const sel = document.getElementById("file") as HTMLSelectElement;
  const del = document.getElementById("delete") as HTMLButtonElement;
  const treeEl = document.getElementById("tree") as HTMLDivElement;
  const viewEl = document.getElementById("view") as HTMLDivElement;

  let runs: Run[] = [];

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
        const div = document.createElement("div");
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
      viewEl.innerHTML = `<h2>💾 checkpoint slot ${seg.slot} — ${seg.areas.length} areas</h2>${areaTable(seg.areas)}`;
      return;
    }

    // Restore: the interesting diff is the pre-merge dump
    // (snapshot areas vs /proc/self/maps at restore time) — it shows the
    // mismatch that killed the restore even if the read loop never ran.
    const rows = diffAreas(seg.savedDump, seg.currentDump);
    const changed = rows.filter((r) => !r.ok).length;
    const readNote =
      seg.areas.length > 0
        ? `<p class="dim">${seg.areas.length} areas read after reallocation</p>`
        : `<p class="mismatch">aborted during reallocation — no areas were read</p>`;

    viewEl.innerHTML = `<h2>↩ restore slot ${seg.slot} — snapshot vs process <span class="${changed ? "mismatch" : ""}">${changed} changed</span></h2>
      ${readNote}
      ${diffTable(rows, "snapshot", "process")}`;
  }

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
    const text = await (await fetch(`/api/file/${name}`)).text();
    runs = parseFile(text);
    renderTree();
    viewEl.innerHTML = "<p class='dim'>select a checkpoint or restore</p>";
  }

  sel.onchange = () => load(sel.value);
  del.onclick = async () => {
    del.disabled = true;
    try {
      await fetch("/api/delete-all", { method: "POST" });
      runs = [];
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

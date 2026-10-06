import { defineConfig } from "vite";
import { readdirSync, readFileSync, rmSync, statSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

// Direct host runs default to VALO_HOME=/tmp/valo-$PID with the structured
// log at <home>/valo-struct.jsonl. Steam-run runs escape the sandbox tmpfs
// via the bind-mounted XDG state dir (flat valo-struct-$$.jsonl from
// scripts/profile).
const RUNS_DIR = "/tmp";
const STATE_DIR = join(homedir(), ".local/state/valo");
const PROFILE_DIR = fileURLToPath(new URL("../out", import.meta.url));

type ListedFile = {
  /** unique list key (run dir name or flat file name) */
  name: string;
  path: string;
  /** file or dir removed by /api/delete-all */
  cleanup: string;
  profile: boolean;
  mtime: number;
};

function listValoFiles(): ListedFile[] {
  const files: ListedFile[] = [];
  const push = (name: string, path: string, cleanup: string, profile: boolean) => {
    try {
      files.push({ name, path, cleanup, profile, mtime: statSync(path).mtimeMs });
    } catch {}
  };

  try {
    for (const f of readdirSync(RUNS_DIR)) {
      if (!f.startsWith("valo-")) continue;
      const runDir = join(RUNS_DIR, f);
      try {
        if (!statSync(runDir).isDirectory()) continue;
        // Keyed by dir name: the file name is identical across runs.
        push(f, join(runDir, "valo-struct.jsonl"), runDir, false);
      } catch {}
    }
  } catch {}

  try {
    for (const f of readdirSync(STATE_DIR)) {
      if (!f.startsWith("valo") || !f.endsWith(".jsonl")) continue;
      const p = join(STATE_DIR, f);
      try {
        if (!statSync(p).isFile()) continue;
        push(f, p, p, false);
      } catch {}
    }
  } catch {}

  push("perf-struct.jsonl", join(PROFILE_DIR, "perf-struct.jsonl"), "", true);

  return files.sort((a, b) => b.mtime - a.mtime);
}

// Vite dev plugin: serve valo files without a separate backend.
const valoApi = () => ({
  name: "valo-api",
  configureServer(server: any) {
    server.middlewares.use((req: any, res: any, next: any) => {
      if (!req.url?.startsWith("/api/")) return next();

      if (req.url === "/api/files") {
        res.setHeader("Content-Type", "application/json");
        res.end(
          JSON.stringify(
            listValoFiles().map(({ name, profile, mtime }) => ({
              name,
              profile,
              mtime,
            })),
          ),
        );
        return;
      }

      if (req.url === "/api/delete-all" && req.method === "POST") {
        let deleted = 0;
        for (const { cleanup, profile } of listValoFiles()) {
          if (profile || !cleanup) continue;
          try {
            rmSync(cleanup, { recursive: true, force: true });
            deleted++;
          } catch {}
        }
        res.setHeader("Content-Type", "application/json");
        res.end(JSON.stringify({ deleted }));
        return;
      }

      const match = req.url.match(/^\/api\/file\/([^/]+)$/);
      if (match) {
        const name = decodeURIComponent(match[1]);
        if (name.includes("..") || name.includes("/")) {
          res.statusCode = 400;
          res.end("bad path");
          return;
        }
        const listed = listValoFiles().find((f) => f.name === name);
        if (!listed) {
          res.statusCode = 404;
          res.end("not found");
          return;
        }
        try {
          res.setHeader("Content-Type", "text/plain");
          res.end(readFileSync(listed.path, "utf8"));
        } catch {
          res.statusCode = 404;
          res.end("not found");
        }
        return;
      }

      next();
    });
  },
});

export default defineConfig({
  plugins: [valoApi()],
});

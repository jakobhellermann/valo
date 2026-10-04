import { defineConfig } from "vite";
import { readdirSync, readFileSync, statSync, unlinkSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

// Direct host runs use the lib defaults (/tmp); steam-run runs escape the
// sandbox tmpfs via the bind-mounted home (XDG state dir).
const EVENTS_DIRS = ["/tmp", join(homedir(), ".local/state/valo")];

type ListedFile = { name: string; dir: string; mtime: number };

function listValoFiles(): ListedFile[] {
  const files: ListedFile[] = [];
  for (const dir of EVENTS_DIRS) {
    let names: string[];
    try {
      names = readdirSync(dir);
    } catch {
      continue;
    }
    for (const f of names) {
      if (!f.startsWith("valo") || !f.endsWith(".jsonl")) continue;
      try {
        const p = join(dir, f);
        if (!statSync(p).isFile()) continue;
        files.push({ name: f, dir, mtime: statSync(p).mtimeMs });
      } catch {}
    }
  }
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
        res.end(JSON.stringify(listValoFiles().map(({ name }) => ({ name }))));
        return;
      }

      if (req.url === "/api/delete-all" && req.method === "POST") {
        let deleted = 0;
        for (const { dir, name } of listValoFiles()) {
          try {
            unlinkSync(join(dir, name));
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
          res.end(readFileSync(join(listed.dir, listed.name), "utf8"));
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

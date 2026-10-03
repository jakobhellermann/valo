import { defineConfig } from "vite";
import { readdirSync, readFileSync, statSync, unlinkSync } from "node:fs";
import { join } from "node:path";

const TMP = "/tmp";

function listValoFiles(): string[] {
  return readdirSync(TMP)
    .filter((f) => f.startsWith("valo"))
    .filter((f) => f.endsWith(".jsonl"))
    .filter((f) => {
      try {
        return statSync(join(TMP, f)).isFile();
      } catch {
        return false;
      }
    })
    .map((f) => ({ name: f, mtime: statSync(join(TMP, f)).mtimeMs }))
    .sort((a, b) => b.mtime - a.mtime)
    .map((f) => f.name);
}

// Vite dev plugin: serve valo files from /tmp without a separate backend.
const valoApi = () => ({
  name: "valo-api",
  configureServer(server: any) {
    server.middlewares.use((req: any, res: any, next: any) => {
      if (!req.url?.startsWith("/api/")) return next();

      if (req.url === "/api/files") {
        res.setHeader("Content-Type", "application/json");
        res.end(JSON.stringify(listValoFiles().map((name) => ({ name }))));
        return;
      }

      if (req.url === "/api/delete-all" && req.method === "POST") {
        let deleted = 0;
        for (const name of listValoFiles()) {
          try {
            unlinkSync(join(TMP, name));
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
        try {
          res.setHeader("Content-Type", "text/plain");
          res.end(readFileSync(join(TMP, name), "utf8"));
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

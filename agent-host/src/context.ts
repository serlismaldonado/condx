// Las instrucciones del proyecto para los agentes: el AGENTS.md (y el CLAUDE.md) de la raíz del repo
// donde trabaja el agente. Claude Code ya carga el CLAUDE.md por su cuenta; los demás proveedores no
// cargan ninguno, así que Condx los agrega a sus instrucciones.

import { existsSync, readFileSync, realpathSync, statSync } from "node:fs";
import { dirname, join } from "node:path";

/** Tope por archivo (en caracteres): un AGENTS.md enorme no se come la ventana del modelo. */
export const MAX_FILE_CHARS = 20_000;

/** La raíz del repo de `cwd` (la carpeta con `.git`, archivo en un worktree o carpeta), o undefined. */
export function repoRoot(cwd: string): string | undefined {
  let dir = cwd;
  for (let i = 0; i < 40; i++) {
    if (existsSync(join(dir, ".git"))) return dir;
    const up = dirname(dir);
    if (up === dir) return undefined;
    dir = up;
  }
  return undefined;
}

/** Las instrucciones del proyecto para un agente que trabaja en `cwd`; vacío si no hay archivos. */
export function projectContext(cwd: string, opts: { claudeCode: boolean }): string {
  if (!cwd) return "";
  const root = repoRoot(cwd);
  if (!root) return "";
  const names = opts.claudeCode ? ["AGENTS.md"] : ["AGENTS.md", "CLAUDE.md"];
  const seen = new Set<string>();
  const parts: string[] = [];
  for (const name of names) {
    const path = join(root, name);
    try {
      if (!statSync(path).isFile()) continue;
      // Un CLAUDE.md que es un enlace al AGENTS.md (o con el mismo texto) va una vez.
      const real = realpathSync(path);
      let text = readFileSync(real, "utf8").trim();
      if (!text || seen.has(real) || seen.has(text)) continue;
      seen.add(real);
      seen.add(text);
      if (text.length > MAX_FILE_CHARS) text = `${text.slice(0, MAX_FILE_CHARS)}\n\n[… cut: the file is longer; read ${name} for the rest]`;
      parts.push(`<project-file name="${name}">\n${text}\n</project-file>`);
    } catch {
      // Sin el archivo (o sin permiso): nada.
    }
  }
  if (parts.length === 0) return "";
  return [
    "## Project instructions",
    "The project's own instructions for coding agents, from its repository. Follow them while you work on this project; Condx's rules above still win if they conflict.",
    ...parts,
  ].join("\n\n");
}

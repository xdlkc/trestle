import { copyFileSync, mkdirSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const srcDir = resolve(here, "src");
const outDir = resolve(here, "..", "src", "static");

mkdirSync(outDir, { recursive: true });
for (const file of ["index.html", "app.js", "styles.css"]) {
  copyFileSync(resolve(srcDir, file), resolve(outDir, file));
  console.log(`Copied ${file} -> ${resolve(outDir, file)}`);
}

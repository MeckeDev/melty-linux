#!/usr/bin/env node
// Zerlegt dist/main.bundle.mjs (esbuild, unminifiziert) in einzelne Quellmodule anhand der
// "// dist-tsc/..." bzw. "// ../../lib/..." Kommentare. Drittanbieter-Code (node_modules, Sentry) wird übersprungen.
// Nutzung: node split-bundle.mjs <main.bundle.mjs> <ausgabeordner>
import { readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { join, dirname } from "node:path";

const [, , input, outDir] = process.argv;
if (!input || !outDir) { console.error("Nutzung: node split-bundle.mjs <bundle> <outdir>"); process.exit(1); }

const lines = readFileSync(input, "utf8").split("\n");
const marker = /^\/\/ (dist-tsc\/apps\/capture\/src\/[^\s]+|\.\.\/\.\.\/lib\/[^\s]+)$/;
const sections = [];
let cur = null;
lines.forEach((line, i) => {
  const m = marker.exec(line);
  if (m) { cur = { name: m[1], start: i + 1, body: [] }; sections.push(cur); }
  else if (/^\/\/ node_modules\//.test(line)) cur = null; // Drittanbieter beginnt
  if (cur) cur.body.push(line);
});

// Gleiche Quelldatei kann mehrfach auftauchen (esbuild verschränkt Module): zusammenführen.
const merged = new Map();
for (const s of sections) {
  const key = s.name.replace(/^dist-tsc\/apps\/capture\/src\//, "capture/").replace(/^\.\.\/\.\.\/lib\//, "lib/");
  const e = merged.get(key) ?? { key, parts: [], lines: 0 };
  e.parts.push(`/* --- Bundle-Zeilen ab ${s.start} --- */\n` + s.body.join("\n"));
  e.lines += s.body.length;
  merged.set(key, e);
}

const index = [];
for (const e of [...merged.values()].sort((a, b) => a.key.localeCompare(b.key))) {
  const file = join(outDir, e.key.replace(/\.(js|ts)$/, "") + ".mjs.txt");
  mkdirSync(dirname(file), { recursive: true });
  writeFileSync(file, e.parts.join("\n\n"));
  index.push({ module: e.key, lines: e.lines });
}
writeFileSync(join(outDir, "INDEX.json"), JSON.stringify(index, null, 1));
const total = index.reduce((n, x) => n + x.lines, 0);
console.log(`${index.length} Module, ${total} Zeilen App-Code (von ${lines.length} im Bundle)`);

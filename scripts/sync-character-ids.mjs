#!/usr/bin/env node
// Fetch upstream MarvelRivalsCharacterIDs.md and regenerate src-tauri/data/character_ids.json.
// Run: node scripts/sync-character-ids.mjs

import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

const __dirname = dirname(fileURLToPath(import.meta.url));
const UPSTREAM =
  "https://raw.githubusercontent.com/donutman07/MarvelRivalsCharacterIDs/main/MarvelRivalsCharacterIDs.md";
const OUT_PATH = resolve(__dirname, "..", "src-tauri", "data", "character_ids.json");

// Their absence means the parser regressed or upstream format changed.
// Fail so cron does not overwrite a healthy committed file.
const CANARY_IDS = ["1011", "1014", "1015"];
// New characters arrive a few at a time, so a legitimate delta is tiny.
const MAX_COUNT_DROP = 20;

function shouldSkipName(name) {
  if (!name) return true;
  if (name.startsWith("????")) return true;
  if (name.startsWith("No Data")) return true;
  if (name === "Upcoming Characters") return true;
  if (name.endsWith(" (Old)")) return true;
  if (name.endsWith(" Bot") || name.includes(" Bot (")) return true;
  if (name.startsWith("Zombie")) return true;
  if (name.includes("Mislabeled")) return true;
  return false;
}

function cleanCell(cell) {
  return cell.trim().replace(/\s+/g, " ");
}

async function main() {
  const res = await fetch(UPSTREAM);
  if (!res.ok) {
    throw new Error(`fetch failed: ${res.status} ${res.statusText}`);
  }
  const md = await res.text();

  /** @type {Map<string, { id: string, name: string, skins: Map<string, string> }>} */
  const byId = new Map();
  let currentId = null;

  for (const rawLine of md.split(/\r?\n/)) {
    const line = rawLine.trim();
    if (!line.startsWith("|")) continue;
    if (/^\|\s*:?-+:?\s*\|/.test(line)) continue;

    // Some upstream rows omit the trailing pipe.
    const rawCells = line.split("|").slice(1);
    if (rawCells.length > 0 && rawCells[rawCells.length - 1].trim() === "") {
      rawCells.pop();
    }
    const cells = rawCells.map(cleanCell);
    while (cells.length < 4) cells.push("");

    const [idCell, nameCell, skinIdCell, skinNameCell] = cells;

    if (/^ID$/i.test(idCell) && /^NAME$/i.test(nameCell)) continue;

    if (idCell && nameCell) {
      if (!/^\d+$/.test(idCell)) {
        currentId = null;
        continue;
      }
      // 4XXX are NPC/zombie/bot IDs, 9XXX are placeholders.
      const idNum = Number(idCell);
      if (idNum < 1000 || idNum >= 4000) {
        currentId = null;
        continue;
      }
      if (shouldSkipName(nameCell)) {
        currentId = null;
        continue;
      }
      currentId = idCell;
      const cleanName = nameCell.replace(/\s*\([^)]*\)\s*$/, "").trim() || nameCell;
      if (!byId.has(currentId)) {
        byId.set(currentId, {
          id: currentId,
          name: cleanName,
          skins: new Map(),
        });
      }
    }

    if (!idCell && !nameCell && skinIdCell && skinNameCell && currentId) {
      if (!/^\d+$/.test(skinIdCell)) continue;
      const cleanSkinName = skinNameCell.replace(/\s*\(.*$/, "").trim() || skinNameCell;
      const entry = byId.get(currentId);
      if (entry && !entry.skins.has(skinIdCell)) {
        entry.skins.set(skinIdCell, cleanSkinName);
      }
    }

    if (idCell && nameCell && skinIdCell && skinNameCell && currentId === idCell) {
      if (/^\d+$/.test(skinIdCell)) {
        const entry = byId.get(currentId);
        const cleanSkinName = skinNameCell.replace(/\s*\(.*$/, "").trim() || skinNameCell;
        if (entry && !entry.skins.has(skinIdCell)) {
          entry.skins.set(skinIdCell, cleanSkinName);
        }
      }
    }
  }

  const characters = {};
  const sortedIds = [...byId.keys()].sort((a, b) => Number(a) - Number(b));
  for (const id of sortedIds) {
    const entry = byId.get(id);
    if (entry.skins.size === 0) continue;
    // Upstream omits the default skin row; inject so the detector can label it.
    const defaultSkinId = `${id}001`;
    if (!entry.skins.has(defaultSkinId)) {
      entry.skins.set(defaultSkinId, "Default");
    }
    const skins = {};
    const sortedSkinIds = [...entry.skins.keys()].sort((a, b) => Number(a) - Number(b));
    for (const skinId of sortedSkinIds) {
      skins[skinId] = entry.skins.get(skinId);
    }
    characters[id] = { name: entry.name, skins };
  }

  const newCount = Object.keys(characters).length;

  const missingCanaries = CANARY_IDS.filter((id) => !characters[id]);
  if (missingCanaries.length > 0) {
    throw new Error(
      `sanity gate: missing canary IDs ${missingCanaries.join(", ")} (${newCount} chars parsed)`
    );
  }

  // Sorted IDs make JSON.stringify a valid equality check.
  let previous = null;
  try {
    previous = JSON.parse(readFileSync(OUT_PATH, "utf8"));
  } catch {
    previous = null;
  }
  const previousCount = previous?.characters ? Object.keys(previous.characters).length : 0;

  if (previousCount > 0 && previousCount - newCount > MAX_COUNT_DROP) {
    throw new Error(
      `sanity gate: character count dropped ${previousCount - newCount} (from ${previousCount} to ${newCount})`
    );
  }

  const newCharsJson = JSON.stringify(characters);
  const prevCharsJson = JSON.stringify(previous?.characters ?? {});
  if (newCharsJson === prevCharsJson) {
    console.log(`no character changes (${newCount} chars, unchanged)`);
    return;
  }

  const output = {
    source: UPSTREAM,
    generated_at: new Date().toISOString(),
    characters,
  };

  writeFileSync(OUT_PATH, JSON.stringify(output, null, 2) + "\n", "utf8");
  console.log(`wrote ${newCount} characters to ${OUT_PATH} (previous: ${previousCount})`);
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});

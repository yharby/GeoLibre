// Downloads and gunzips the DuckDB spatial extension (v1.5.4) for every target
// platform into the Tauri resources tree. Run before `tauri:build`.
import { createWriteStream } from "node:fs";
import { mkdir, rm } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { createGunzip } from "node:zlib";
import { pipeline } from "node:stream/promises";

const DUCKDB_VERSION = "v1.5.4";
const PLATFORMS = ["osx_arm64", "osx_amd64", "windows_amd64", "linux_amd64", "linux_arm64"];
const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const outBase = join(root, "apps/geolibre-desktop/src-tauri/resources/duckdb");

for (const platform of PLATFORMS) {
  const url = `http://extensions.duckdb.org/${DUCKDB_VERSION}/${platform}/spatial.duckdb_extension.gz`;
  const outDir = join(outBase, platform);
  const outFile = join(outDir, "spatial.duckdb_extension");
  await mkdir(outDir, { recursive: true });
  process.stdout.write(`Fetching ${platform} ... `);
  const res = await fetch(url);
  if (!res.ok) {
    await rm(outFile, { force: true });
    throw new Error(`Failed ${platform}: HTTP ${res.status} from ${url}`);
  }
  await pipeline(res.body, createGunzip(), createWriteStream(outFile));
  console.log("done");
}
console.log("All spatial extensions fetched.");

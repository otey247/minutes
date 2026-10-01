import { createHash } from "crypto";
import { gunzip } from "zlib";
import { promisify } from "util";
import { basename, join } from "path";
import {
  chmod,
  copyFile,
  mkdir,
  mkdtemp,
  readFile,
  rename,
  rm,
  stat,
  writeFile,
} from "fs/promises";

type ExecFileAsync = (
  file: string,
  args: readonly string[],
  options?: { timeout?: number; env?: NodeJS.ProcessEnv }
) => Promise<unknown>;

const gunzipAsync = promisify(gunzip);

export const MACOS_SHERPA_ARCHIVE = "minutes-macos-arm64-sherpa.tar.gz";
export const MACOS_BARE_BINARY = "minutes-macos-arm64";

export class ReleaseAssetUnavailableError extends Error {
  readonly assetName: string;

  constructor(assetName: string, message: string) {
    super(message);
    this.name = "ReleaseAssetUnavailableError";
    this.assetName = assetName;
  }
}

export type Sha256Entry = {
  filename: string;
  sha256: string;
};

export function parseSha256Sums(raw: string): Sha256Entry[] {
  const entries: Sha256Entry[] = [];

  for (const line of raw.split(/\r?\n/)) {
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith("#")) continue;

    const match = trimmed.match(/^([a-fA-F0-9]{64})[ \t]+[* ]?(.+)$/);
    if (!match) continue;

    const [, sha256, filename] = match;
    entries.push({
      filename: filename.trim(),
      sha256: sha256.toLowerCase(),
    });
  }

  return entries;
}

export function findSha256ForAsset(raw: string, assetName: string): string | null {
  for (const entry of parseSha256Sums(raw)) {
    if (entry.filename === assetName || basename(entry.filename) === assetName) {
      return entry.sha256;
    }
  }
  return null;
}

export async function computeFileSha256(path: string): Promise<string> {
  const bytes = await readFile(path);
  return createHash("sha256").update(bytes).digest("hex");
}

export async function verifyDownloadedAsset(
  path: string,
  expectedSha256: string
): Promise<void> {
  const actual = await computeFileSha256(path);
  if (actual !== expectedSha256.toLowerCase()) {
    throw new Error(
      `checksum mismatch: expected ${expectedSha256.toLowerCase()}, got ${actual}`
    );
  }
}

function tarString(header: Buffer, offset: number, length: number): string {
  const field = header.subarray(offset, offset + length);
  const nul = field.indexOf(0);
  return field.subarray(0, nul === -1 ? field.length : nul).toString("utf8").trim();
}

function tarSize(header: Buffer): number {
  const raw = tarString(header, 124, 12).replace(/\0/g, "").trim();
  if (!/^[0-7]+$/.test(raw)) {
    throw new Error(`invalid tar entry size ${JSON.stringify(raw)}`);
  }
  return Number.parseInt(raw, 8);
}

/**
 * Extract the two files from the macOS sherpa release archive.
 *
 * The release archive has one fixed directory and two fixed regular files.
 * Keeping this reader deliberately narrow avoids a package dependency and,
 * more importantly, prevents path traversal, links, devices, or unexpected
 * files from ever being materialized. The archive itself is SHA-256 verified
 * before this function is called.
 */
export async function extractMacSherpaArchive(options: {
  archivePath: string;
  destDir: string;
}): Promise<{ binaryPath: string; pluginPath: string }> {
  const compressed = await readFile(options.archivePath);
  const tar = await gunzipAsync(compressed);
  const expected = new Map<string, Buffer>();
  const root = "minutes-macos-arm64-sherpa";
  const expectedNames = new Set([
    `${root}/minutes`,
    `${root}/libminutes_sherpa.dylib`,
  ]);

  let offset = 0;
  while (offset + 512 <= tar.length) {
    const header = tar.subarray(offset, offset + 512);
    if (header.every((byte) => byte === 0)) break;

    const name = tarString(header, 0, 100);
    const prefix = tarString(header, 345, 155);
    const archivePath = `${prefix ? `${prefix}/` : ""}${name}`.replace(/^\.\//, "");
    const size = tarSize(header);
    const type = String.fromCharCode(header[156] || 0);
    const dataStart = offset + 512;
    const dataEnd = dataStart + size;
    if (dataEnd > tar.length) {
      throw new Error(`truncated tar entry ${archivePath}`);
    }

    if (type === "\0" || type === "0") {
      if (!expectedNames.has(archivePath)) {
        throw new Error(`unexpected file in macOS sherpa archive: ${archivePath}`);
      }
      if (expected.has(archivePath)) {
        throw new Error(`duplicate file in macOS sherpa archive: ${archivePath}`);
      }
      expected.set(archivePath, Buffer.from(tar.subarray(dataStart, dataEnd)));
    } else if (type !== "5" && type !== "x" && type !== "g") {
      throw new Error(`unsupported tar entry type ${JSON.stringify(type)} for ${archivePath}`);
    }

    offset = dataStart + Math.ceil(size / 512) * 512;
  }

  for (const name of expectedNames) {
    if (!expected.has(name)) {
      throw new Error(`macOS sherpa archive is missing ${name}`);
    }
  }

  await mkdir(options.destDir, { recursive: true });
  const binaryPath = join(options.destDir, "minutes");
  const pluginPath = join(options.destDir, "libminutes_sherpa.dylib");
  await writeFile(binaryPath, expected.get(`${root}/minutes`)!, { mode: 0o755 });
  await writeFile(pluginPath, expected.get(`${root}/libminutes_sherpa.dylib`)!, {
    mode: 0o755,
  });
  return { binaryPath, pluginPath };
}

/**
 * Commit a verified binary/plugin pair. The binary rename is the commit point:
 * it is never touched until the staged binary passes `--version`. If that
 * final rename fails, restore the previous plugin so callers never observe a
 * completed-but-partial install.
 */
export async function atomicReplaceMacSherpaFiles(options: {
  stagedBinaryPath: string;
  stagedPluginPath: string;
  targetBinaryPath: string;
  targetPluginPath: string;
  backupDir: string;
}): Promise<void> {
  const pluginBackup = join(options.backupDir, "previous-libminutes_sherpa.dylib");
  let hadPlugin = false;
  try {
    await stat(options.targetPluginPath);
    hadPlugin = true;
    await copyFile(options.targetPluginPath, pluginBackup);
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
  }

  let pluginReplaced = false;
  try {
    await rename(options.stagedPluginPath, options.targetPluginPath);
    pluginReplaced = true;
    await rename(options.stagedBinaryPath, options.targetBinaryPath);
  } catch (error) {
    if (pluginReplaced) {
      if (hadPlugin) {
        await rename(pluginBackup, options.targetPluginPath).catch(() => {});
      } else {
        await rm(options.targetPluginPath, { force: true }).catch(() => {});
      }
    }
    throw error;
  } finally {
    await rm(pluginBackup, { force: true }).catch(() => {});
  }
}

export type MacSherpaInstallResult = {
  assetName: string;
  usedFallback: boolean;
};

/** Install the signed macOS sherpa archive, falling back only when unavailable. */
export async function installMacSherpaArchiveWithFallback(options: {
  installDir: string;
  execFileAsync: ExecFileAsync;
  baseUrl?: string;
  log?: (message: string) => void;
  downloadAsset?: typeof downloadReleaseBinaryWithChecksum;
  extractArchive?: typeof extractMacSherpaArchive;
  verifyExecutable?: (binaryPath: string) => Promise<void>;
}): Promise<MacSherpaInstallResult> {
  const log = options.log ?? (() => {});
  const downloadAsset = options.downloadAsset ?? downloadReleaseBinaryWithChecksum;
  const extractArchive = options.extractArchive ?? extractMacSherpaArchive;
  await mkdir(options.installDir, { recursive: true });
  const temporaryDir = await mkdtemp(join(options.installDir, ".minutes-sherpa-install-"));

  try {
    const archivePath = join(temporaryDir, MACOS_SHERPA_ARCHIVE);
    try {
      await downloadAsset({
        binaryName: MACOS_SHERPA_ARCHIVE,
        targetPath: archivePath,
        execFileAsync: options.execFileAsync,
        baseUrl: options.baseUrl,
      });
    } catch (error) {
      if (!(error instanceof ReleaseAssetUnavailableError)) throw error;

      log(
        `[Minutes] ${MACOS_SHERPA_ARCHIVE} is unavailable in this release (${error.message}); ` +
          `falling back to the bare ${MACOS_BARE_BINARY} asset`
      );
      const targetBinaryPath = join(options.installDir, "minutes");
      await downloadAsset({
        binaryName: MACOS_BARE_BINARY,
        targetPath: targetBinaryPath,
        execFileAsync: options.execFileAsync,
        baseUrl: options.baseUrl,
      });
      await chmod(targetBinaryPath, 0o755);
      return { assetName: MACOS_BARE_BINARY, usedFallback: true };
    }

    const extractedDir = join(temporaryDir, "extracted");
    await mkdir(extractedDir, { recursive: true });
    const { binaryPath, pluginPath } = await extractArchive({
      archivePath,
      destDir: extractedDir,
    });
    await chmod(binaryPath, 0o755);

    const verifyExecutable = options.verifyExecutable ?? (async (path: string) => {
      await options.execFileAsync(path, ["--version"], { timeout: 5000 });
    });
    await verifyExecutable(binaryPath);

    await atomicReplaceMacSherpaFiles({
      stagedBinaryPath: binaryPath,
      stagedPluginPath: pluginPath,
      targetBinaryPath: join(options.installDir, "minutes"),
      targetPluginPath: join(options.installDir, "libminutes_sherpa.dylib"),
      backupDir: temporaryDir,
    });
    return { assetName: MACOS_SHERPA_ARCHIVE, usedFallback: false };
  } finally {
    await rm(temporaryDir, { recursive: true, force: true }).catch(() => {});
  }
}

/**
 * Extract a zip on Windows via PowerShell's Expand-Archive.
 *
 * The paths travel in the environment, never in the command text. Two ways to
 * get this wrong, both of which shipped:
 *
 * - Interpolating into a single-quoted PowerShell string breaks on a home
 *   directory containing an apostrophe (`C:\Users\O'Brien`).
 * - Passing them as trailing arguments and reading `$args[0]` does not work at
 *   all: `powershell -Command` appends trailing arguments to the command text
 *   and leaves `$args` empty (only `-File` binds them), so `$args[0]` is
 *   `$null` and `Expand-Archive` rejects the parameter on every machine.
 *
 * `$env:` lookups are read at runtime and are never parsed as script, so no
 * value of either path can alter the command.
 */
export async function extractZipWithPowerShell(options: {
  archivePath: string;
  destDir: string;
  execFileAsync: ExecFileAsync;
}): Promise<void> {
  const { archivePath, destDir, execFileAsync } = options;
  await execFileAsync(
    "powershell",
    [
      "-NoProfile",
      "-NonInteractive",
      "-Command",
      "Expand-Archive -Path $env:MINUTES_ZIP_PATH -DestinationPath $env:MINUTES_ZIP_DEST -Force",
    ],
    {
      timeout: 120000,
      env: {
        ...process.env,
        MINUTES_ZIP_PATH: archivePath,
        MINUTES_ZIP_DEST: destDir,
      },
    }
  );
}

/** Allow Windows audio services to load the CLI and DLLs from the install bin.
 * The parent ~/.minutes directory contains private data and must stay private.
 */
export async function grantWindowsSystemBinaryAccess(options: {
  installDir: string;
  execFileAsync: ExecFileAsync;
}): Promise<void> {
  await options.execFileAsync(
    "icacls.exe",
    [options.installDir, "/grant", "*S-1-5-18:(OI)(CI)(RX)"],
    { timeout: 10000 }
  );
}

export async function downloadReleaseBinaryWithChecksum(options: {
  binaryName: string;
  targetPath: string;
  execFileAsync: ExecFileAsync;
  baseUrl?: string;
}): Promise<void> {
  const { binaryName, targetPath, execFileAsync } = options;
  const baseUrl =
    options.baseUrl ?? "https://github.com/silverstein/minutes/releases/latest/download";
  const sumsUrl = `${baseUrl}/SHA256SUMS.txt`;
  const binaryUrl = `${baseUrl}/${binaryName}`;
  const tempSumsPath = `${targetPath}.SHA256SUMS.tmp`;
  const tempBinaryPath = `${targetPath}.download`;

  try {
    await execFileAsync("curl", ["-fSL", "-o", tempSumsPath, sumsUrl], {
      timeout: 30000,
    });
    const sums = await readFile(tempSumsPath, "utf8");
    const expectedSha256 = findSha256ForAsset(sums, binaryName);
    if (!expectedSha256) {
      throw new ReleaseAssetUnavailableError(
        binaryName,
        `SHA256SUMS.txt has no entry for ${binaryName}`
      );
    }

    try {
      await execFileAsync("curl", ["-fSL", "-o", tempBinaryPath, binaryUrl], {
        timeout: 120000,
      });
    } catch (error) {
      const stderr = String((error as { stderr?: unknown })?.stderr ?? "");
      if (/\b404\b|not found/i.test(stderr)) {
        throw new ReleaseAssetUnavailableError(
          binaryName,
          `${binaryName} returned HTTP 404`
        );
      }
      throw error;
    }
    await verifyDownloadedAsset(tempBinaryPath, expectedSha256);
    await rename(tempBinaryPath, targetPath);
  } catch (error) {
    await rm(tempBinaryPath, { force: true }).catch(() => {});
    throw error;
  } finally {
    await rm(tempSumsPath, { force: true }).catch(() => {});
  }
}

import { Database } from "bun:sqlite";
import * as fs from "node:fs/promises";
import * as path from "node:path";
import {
	AstMatchStrictness,
	type AstReplaceChange,
	type AstReplaceFileChange,
	astEdit,
	astGrep,
	type FileType,
	glob as nativeGlob,
	grep as nativeGrep,
	listWorkspace as nativeListWorkspace,
	Shell,
	notebookToEditableText,
	summarizeCode,
} from "@oh-my-pi/pi-natives";
import { isEnoent, readImageMetadata, untilAborted } from "@oh-my-pi/pi-utils";
import { BadRequestError, EtagMismatchError, NotFoundError } from "@oh-my-pi/rwp-client";
import { Settings } from "../config/settings";
import { DapClient } from "../dap/client";
import type { DapResolvedAdapter } from "../dap/types";
import { resetVmContext } from "../eval/js/context-manager";
import { executeJs } from "../eval/js/executor";
import { disposeKernelSessionsByOwner, executePython, type PythonResult } from "../eval/py/executor";
import {
	getOrCreateClient,
	sendNotification as sendLspNotification,
	sendRequest as sendLspRequest,
	shutdownClientInstance,
} from "../lsp/client";
import type { LspClient, ServerConfig } from "../lsp/types";
import type { ToolSession } from "../tools";
import {
	acquireBrowser,
	type BrowserHandle,
	type BrowserKind,
	holdBrowser,
	releaseBrowser,
} from "../tools/browser/registry";
import {
	deleteRowByKey,
	deleteRowByRowId,
	executeReadQuery,
	getRowByKey,
	getRowByRowId,
	insertRow,
	listTables,
	queryRows,
	resolveTableRowLookup,
	updateRowByKey,
	updateRowByRowId,
} from "../tools/sqlite-reader";
import { ToolError } from "../tools/tool-errors";
import { convertBufferWithMarkit } from "../utils/markit";
import {
	type Backend,
	BackendEditPipeline,
	type BrowserBackend,
	type DapBackend,
	type EditBackend,
	type FsBackend,
	type JsonRpcChannel,
	type KernelBackend,
	type LspBackend,
	type ShellBackend,
	type SqliteBackend,
} from "./backend";
import type * as T from "./types";

const AST_MATCH_STRICTNESS: Record<"smart" | "relaxed" | "strict", AstMatchStrictness> = {
	smart: AstMatchStrictness.Smart,
	relaxed: AstMatchStrictness.Relaxed,
	strict: AstMatchStrictness.Ast,
};

const textEncoder = new TextEncoder();

function hashBytes(bytes: Uint8Array): string {
	return Bun.hash(bytes).toString(16);
}

function toLocalFsError(error: unknown, filePath: string): unknown {
	if (isEnoent(error)) {
		return new NotFoundError(404, { code: "not-found", message: `Path not found: ${filePath}` });
	}
	if ((error as NodeJS.ErrnoException | undefined)?.code === "EEXIST") {
		return new BadRequestError(400, { code: "bad-request", message: `Path already exists: ${filePath}` });
	}
	return error;
}

function getAstEditOps(req: T.EditAstRequest): NonNullable<T.EditAstRequest["ops"]> {
	const ops = req.ops ?? req.rules;
	if (!ops || ops.length === 0) {
		throw new BadRequestError(400, {
			code: "bad-request",
			message: "editAst requires at least one rewrite op",
		});
	}
	return ops;
}

function buildAstEditDiff(
	changes: Array<{ path: string; before: string; after: string; startLine: number }>,
	filePath: string,
): string {
	return changes
		.filter(change => change.path === filePath)
		.flatMap(change => {
			const beforeLines = change.before.length > 0 ? change.before.split("\n") : [];
			const afterLines = change.after.length > 0 ? change.after.split("\n") : [];
			const diffLines: string[] = [];
			for (let index = 0; index < beforeLines.length; index += 1) {
				diffLines.push(`-${change.startLine + index}|${beforeLines[index] ?? ""}`);
			}
			for (let index = 0; index < afterLines.length; index += 1) {
				diffLines.push(`+${change.startLine + index}|${afterLines[index] ?? ""}`);
			}
			return diffLines;
		})
		.join("\n");
}
function splitAstEditLines(text: string): string[] {
	if (text.length === 0) return [];
	const normalized = normalizeNewlines(text);
	const lines = normalized.split("\n");
	if (normalized.endsWith("\n")) {
		lines.pop();
	}
	return lines;
}

function applyAstPreviewChanges(sourceText: string, changes: AstReplaceChange[]): string {
	if (changes.length === 0) return sourceText;
	const sourceBytes = Buffer.from(sourceText, "utf8");
	const sorted = [...changes].sort((left, right) => right.byteStart - left.byteStart);
	const segments: Buffer[] = [];
	let cursor = sourceBytes.length;
	for (const change of sorted) {
		segments.unshift(sourceBytes.subarray(change.byteEnd, cursor));
		segments.unshift(Buffer.from(change.after, "utf8"));
		cursor = change.byteStart;
	}
	segments.unshift(sourceBytes.subarray(0, cursor));
	return Buffer.concat(segments).toString("utf8");
}

function buildAstStructuredFileChange(
	path: string,
	replacements: number,
	changes: AstReplaceChange[],
	beforeText: string,
	afterText: string,
): T.AstEditFileChange {
	return {
		path,
		replacements,
		beforeLines: splitAstEditLines(beforeText),
		afterLines: splitAstEditLines(afterText),
		hunks: changes.map(change => ({
			beforeStart: change.startLine,
			beforeLines: splitAstEditLines(change.before),
			afterLines: splitAstEditLines(change.after),
		})),
	};
}

function buildAstStructuredFileChanges(
	fileChanges: AstReplaceFileChange[],
	changes: AstReplaceChange[],
	originals: Map<string, string>,
	nextTexts?: Map<string, string>,
): T.AstEditFileChange[] {
	const changesByFile = new Map<string, AstReplaceChange[]>();
	for (const change of changes) {
		const fileChangesForPath = changesByFile.get(change.path);
		if (fileChangesForPath) {
			fileChangesForPath.push(change);
			continue;
		}
		changesByFile.set(change.path, [change]);
	}
	for (const fileChangesForPath of changesByFile.values()) {
		fileChangesForPath.sort((left, right) => left.byteStart - right.byteStart);
	}
	return fileChanges.map(fileChange => {
		const beforeText = originals.get(fileChange.path) ?? "";
		const perFileChanges = changesByFile.get(fileChange.path) ?? [];
		const afterText = nextTexts?.get(fileChange.path) ?? applyAstPreviewChanges(beforeText, perFileChanges);
		return buildAstStructuredFileChange(fileChange.path, fileChange.count, perFileChanges, beforeText, afterText);
	});
}

let fflateModulePromise: Promise<typeof import("fflate")> | undefined;
async function loadFflate(): Promise<typeof import("fflate")> {
	if (!fflateModulePromise) fflateModulePromise = import("fflate");
	return fflateModulePromise;
}

type ArchiveFormat = T.ArchiveEntriesResult["format"];

function normalizeNewlines(text: string): string {
	return text.replaceAll("\r\n", "\n").replaceAll("\r", "\n");
}

function detectEol(text: string): "LF" | "CRLF" | "CR" {
	let crlf = 0;
	let lf = 0;
	let cr = 0;
	for (let index = 0; index < text.length; index += 1) {
		const ch = text[index];
		if (ch === "\r") {
			if (text[index + 1] === "\n") {
				crlf += 1;
				index += 1;
			} else {
				cr += 1;
			}
			continue;
		}
		if (ch === "\n") {
			lf += 1;
		}
	}
	if (crlf >= lf && crlf >= cr && crlf > 0) return "CRLF";
	if (lf >= cr && lf > 0) return "LF";
	if (cr > 0) return "CR";
	return "LF";
}

function eolString(eol: "LF" | "CRLF" | "CR"): string {
	switch (eol) {
		case "CRLF":
			return "\r\n";
		case "CR":
			return "\r";
		default:
			return "\n";
	}
}

function toReadLinesResult(text: string, startLine: number, etag: string | null, truncated = false): T.ReadLinesResult {
	const bom = text.startsWith("\uFEFF");
	const withoutBom = bom ? text.slice(1) : text;
	const normalized = normalizeNewlines(withoutBom);
	const allLines = normalized.split("\n");
	return {
		lines: allLines,
		startLine,
		etag,
		eol: detectEol(text),
		bom,
		truncated,
		totalLines: allLines.length,
	};
}

function applyLineRange(
	lines: string[],
	range: { start: number; end: number } | undefined,
): { startLine: number; lines: string[] } {
	if (!range) {
		return { startLine: 1, lines };
	}
	const startLine = Math.max(1, range.start);
	const endLine = Math.max(startLine, range.end);
	return {
		startLine,
		lines: lines.slice(startLine - 1, endLine),
	};
}

function normalizeArchiveLookupPath(rawPath?: string): string | undefined {
	if (!rawPath) return "";

	const parts = rawPath.replaceAll("\\", "/").split("/");
	const normalizedParts: string[] = [];
	for (const part of parts) {
		if (!part || part === ".") continue;
		if (part === "..") return undefined;
		normalizedParts.push(part);
	}

	return normalizedParts.join("/");
}

function normalizeArchiveEntryPath(rawPath: string): string | undefined {
	const normalized = normalizeArchiveLookupPath(rawPath);
	return normalized ? normalized : undefined;
}

function normalizeArchiveWritePath(rawPath: string): string {
	const normalized = normalizeArchiveLookupPath(rawPath);
	if (!normalized) {
		throw new ToolError("Archive write path must target a file inside the archive");
	}
	if (rawPath.endsWith("/") || rawPath.endsWith("\\")) {
		throw new ToolError("Archive write path must target a file, not a directory");
	}
	return normalized;
}

function detectArchiveFormat(filePath: string): ArchiveFormat {
	const normalized = filePath.toLowerCase();
	if (normalized.endsWith(".tar.gz") || normalized.endsWith(".tgz")) return "tar.gz";
	if (normalized.endsWith(".tar")) return "tar";
	if (normalized.endsWith(".zip")) return "zip";
	throw new ToolError(`Unsupported archive format: ${filePath}`);
}

function insertArchiveDirectoryEntries(entries: Map<string, T.ArchiveEntry>, filePath: string): void {
	const parts = filePath.split("/");
	for (let index = 1; index < parts.length; index += 1) {
		const dirPath = parts.slice(0, index).join("/");
		if (entries.has(dirPath)) continue;
		entries.set(dirPath, {
			path: dirPath,
			kind: "dir",
			size: 0,
			mtimeMs: null,
			compressedSize: null,
		});
	}
}

const ARCHIVE_CACHE_LIMIT = 32;

interface ArchiveCacheEntry {
	format: ArchiveFormat;
	entries: T.ArchiveEntry[];
	files: Map<string, File | Uint8Array>;
	refs: number;
}

const archiveReadCache = new Map<string, ArchiveCacheEntry>();

function archiveCacheKey(filePath: string, stat: { mtimeMs: number; size: number }): string {
	return `${filePath}:${stat.mtimeMs}:${stat.size}`;
}

function rememberArchiveCache(key: string, entry: ArchiveCacheEntry): ArchiveCacheEntry {
	archiveReadCache.delete(key);
	archiveReadCache.set(key, entry);
	if (archiveReadCache.size > ARCHIVE_CACHE_LIMIT) {
		for (const [oldestKey, oldestEntry] of archiveReadCache) {
			if (archiveReadCache.size <= ARCHIVE_CACHE_LIMIT) break;
			if (oldestEntry.refs === 0) archiveReadCache.delete(oldestKey);
		}
	}
	return entry;
}

function retainArchiveCache(entry: ArchiveCacheEntry): ArchiveCacheEntry {
	entry.refs += 1;
	return entry;
}

function releaseArchiveCache(entry: ArchiveCacheEntry): void {
	if (entry.refs > 0) {
		entry.refs -= 1;
	}
}

function invalidateArchiveCache(filePath: string): void {
	for (const key of archiveReadCache.keys()) {
		if (key.startsWith(`${filePath}:`)) {
			archiveReadCache.delete(key);
		}
	}
}

function withPrefixedPath(baseRelative: string, childPath: string): string {
	const normalizedChild = childPath.replaceAll("\\", "/");
	if (baseRelative === "" || baseRelative === ".") {
		return normalizedChild;
	}
	return path.posix.join(baseRelative.replaceAll("\\", "/"), normalizedChild);
}

function mapFileType(fileType: FileType | undefined): "file" | "dir" | "symlink" | "other" {
	switch (fileType) {
		case 1:
			return "file";
		case 2:
			return "dir";
		case 3:
			return "symlink";
		default:
			return "other";
	}
}

function mapRequestedFileType(type: "file" | "dir" | "symlink" | undefined): FileType | undefined {
	switch (type) {
		case "file":
			return 1;
		case "dir":
			return 2;
		case "symlink":
			return 3;
		default:
			return undefined;
	}
}

function depthOfRelativePath(relativePath: string): number {
	if (relativePath === "" || relativePath === ".") return 0;
	return relativePath.split("/").length - 1;
}

function makeDbColumns(db: Database, table: string): Array<{ name: string; type: string }> {
	return db
		.prepare<{ name: string; type: string }, []>(`PRAGMA table_info(${JSON.stringify(table)})`)
		.all()
		.map(column => ({ name: column.name, type: column.type }));
}

function normalizeDbValue(value: unknown): import("@oh-my-pi/rwp-client").JsonValue {
	if (value === null || typeof value === "string" || typeof value === "number" || typeof value === "boolean") {
		return value;
	}
	if (Array.isArray(value)) {
		return value.map(item => normalizeDbValue(item));
	}
	if (typeof value === "object") {
		const output: Record<string, import("@oh-my-pi/rwp-client").JsonValue | undefined> = {};
		for (const [key, entry] of Object.entries(value)) {
			output[key] = normalizeDbValue(entry);
		}
		return output;
	}
	return value == null ? null : String(value);
}
function makeDbResponse(
	rows: Record<string, unknown>[],
	columns: Array<{ name: string; type: string }>,
	rowidColumn?: string | null,
): T.ReadDbResponse {
	return {
		rows: rows.map(row =>
			Object.fromEntries(Object.entries(row).map(([key, value]) => [key, normalizeDbValue(value)])),
		),
		columns,
		rowid_column: rowidColumn,
	};
}

function mapKernelState(result: PythonResult | { exitCode: number | undefined } | undefined): T.KernelStatus["state"] {
	if (result?.exitCode === 1) return "error";
	return "ready";
}

function makeToolSession(cwd: string, backend: Backend): ToolSession {
	return {
		cwd,
		hasUI: false,
		backend,
		enableLsp: false,
		getSessionFile: () => null,
		getSessionSpawns: () => "*",
		settings: Settings.isolated(),
	};
}

class AsyncQueue<TValue> {
	#items: TValue[] = [];
	#resolvers: Array<(value: IteratorResult<TValue>) => void> = [];
	#done = false;
	#error: unknown = undefined;

	push(value: TValue): void {
		if (this.#done) return;
		const resolve = this.#resolvers.shift();
		if (resolve) {
			resolve({ value, done: false });
			return;
		}
		this.#items.push(value);
	}

	finish(error?: unknown): void {
		if (this.#done) return;
		this.#done = true;
		this.#error = error;
		for (const resolve of this.#resolvers.splice(0)) {
			resolve({ value: undefined, done: true });
		}
	}

	async next(): Promise<IteratorResult<TValue>> {
		if (this.#items.length > 0) {
			const value = this.#items.shift();
			if (value !== undefined) {
				return { value, done: false };
			}
		}
		if (this.#done) {
			if (this.#error !== undefined) {
				throw this.#error;
			}
			return { value: undefined, done: true };
		}
		return await new Promise<IteratorResult<TValue>>(resolve => {
			this.#resolvers.push(resolve);
		});
	}

	[Symbol.asyncIterator](): AsyncIterator<TValue> {
		return {
			next: () => this.next(),
		};
	}
}

class LocalFsBackend implements FsBackend {
	constructor(private readonly cwd: string) {}

	#resolvePath(filePath: string): string {
		return path.isAbsolute(filePath) ? filePath : path.resolve(this.cwd, filePath);
	}

	async #readBytes(filePath: string): Promise<{ bytes: Uint8Array; etag: string }> {
		const bytes = await Bun.file(filePath).bytes();
		return { bytes, etag: hashBytes(bytes) };
	}
	async #writeFileAtomically(absolutePath: string, bytes: Uint8Array): Promise<void> {
		const tmpPath = `${absolutePath}.tmp-${crypto.randomUUID()}`;
		await Bun.write(tmpPath, bytes);
		await fs.rename(tmpPath, absolutePath);
	}

	async #readArchiveCacheEntry(
		absolutePath: string,
		format: ArchiveFormat,
		signal?: AbortSignal,
		opts?: { retain?: boolean },
	): Promise<ArchiveCacheEntry> {
		signal?.throwIfAborted();
		const stat = await fs.stat(absolutePath);
		const key = archiveCacheKey(absolutePath, stat);
		const cached = archiveReadCache.get(key);
		if (cached) {
			const retained = opts?.retain ? retainArchiveCache(cached) : cached;
			return rememberArchiveCache(key, retained);
		}
		const entries = new Map<string, T.ArchiveEntry>();
		const files = new Map<string, File | Uint8Array>();
		if (format === "zip") {
			const bytes = await Bun.file(absolutePath).bytes();
			const { unzipSync } = await loadFflate();
			const archiveFiles = unzipSync(bytes);
			for (const [rawPath, fileBytes] of Object.entries(archiveFiles)) {
				const normalizedPath = normalizeArchiveEntryPath(rawPath);
				if (!normalizedPath) continue;
				const isDirectory = rawPath.endsWith("/") || rawPath.endsWith("\\");
				if (!isDirectory) {
					insertArchiveDirectoryEntries(entries, normalizedPath);
					files.set(normalizedPath, fileBytes);
				}
				entries.set(normalizedPath, {
					path: normalizedPath,
					kind: isDirectory ? "dir" : "file",
					size: isDirectory ? 0 : fileBytes.byteLength,
					mtimeMs: null,
					compressedSize: null,
				});
			}
		} else {
			const archive = new Bun.Archive(await Bun.file(absolutePath).bytes());
			const archiveFiles = await archive.files();
			for (const [rawPath, file] of archiveFiles) {
				const normalizedPath = normalizeArchiveEntryPath(rawPath);
				if (!normalizedPath) continue;
				insertArchiveDirectoryEntries(entries, normalizedPath);
				entries.set(normalizedPath, {
					path: normalizedPath,
					kind: "file",
					size: file.size,
					mtimeMs: file.lastModified > 0 ? file.lastModified : null,
					compressedSize: null,
				});
				files.set(normalizedPath, file);
			}
		}
		const created: ArchiveCacheEntry = {
			format,
			entries: [...entries.values()].sort((left, right) => left.path.localeCompare(right.path)),
			files,
			refs: opts?.retain ? 1 : 0,
		};
		return rememberArchiveCache(key, created);
	}

	async #readArchiveFileMap(
		absolutePath: string,
		format: ArchiveFormat,
		signal?: AbortSignal,
	): Promise<Map<string, File | Uint8Array>> {
		return (await this.#readArchiveCacheEntry(absolutePath, format, signal)).files;
	}

	async readLines(filePath: string, opts?: T.ReadLinesOptions): Promise<T.ReadLinesResult> {
		const absolutePath = this.#resolvePath(filePath);
		let text: string;
		let truncated = false;
		if (opts?.maxLines !== undefined || opts?.maxBytes !== undefined) {
			const chunks: Uint8Array[] = [];
			let totalBytes = 0;
			let totalLines = 0;
			for await (const chunk of Bun.file(absolutePath).stream()) {
				const bytes = chunk instanceof Uint8Array ? chunk : new Uint8Array(chunk);
				if (opts.maxBytes !== undefined && totalBytes + bytes.byteLength > opts.maxBytes) {
					truncated = true;
					break;
				}
				const chunkText = new TextDecoder().decode(bytes);
				const chunkLines = chunkText.split("\n").length - 1;
				if (opts.maxLines !== undefined && totalLines + chunkLines > opts.maxLines) {
					truncated = true;
					break;
				}
				chunks.push(bytes);
				totalBytes += bytes.byteLength;
				totalLines += chunkLines;
			}
			text = Buffer.concat(chunks.map(chunk => Buffer.from(chunk))).toString();
		} else {
			text = await Bun.file(absolutePath).text();
		}
		const bytes = textEncoder.encode(text);
		const full = toReadLinesResult(text, 1, hashBytes(bytes), truncated);
		const ranged = applyLineRange(full.lines, opts?.range);
		const firstLine = ranged.lines[0];
		if (full.bom && ranged.startLine === 1 && firstLine?.startsWith("\uFEFF")) {
			ranged.lines[0] = firstLine.slice(1);
		}
		return {
			lines: ranged.lines,
			startLine: ranged.startLine,
			etag: full.etag,
			eol: full.eol,
			bom: full.bom,
			truncated: full.truncated,
			totalLines: full.totalLines,
		};
	}

	async readBlob(filePath: string, opts?: T.ReadBlobOptions): Promise<T.ReadBlobResult> {
		opts?.signal?.throwIfAborted();
		const absolutePath = this.#resolvePath(filePath);
		const file = Bun.file(absolutePath);
		if (opts?.sizeOnly) {
			const { bytes, etag } = await this.#readBytes(absolutePath);
			return {
				bytes: new Uint8Array(0),
				size: bytes.byteLength,
				etag,
				contentType: file.type || undefined,
			};
		}
		const bytes = await file.bytes();
		const ranged = opts?.range ? bytes.slice(opts.range.start, opts.range.end + 1) : bytes;
		return {
			bytes: ranged,
			size: bytes.byteLength,
			etag: hashBytes(bytes),
			contentType: file.type || undefined,
		};
	}

	async stat(filePath: string, opts?: { signal?: AbortSignal; followSymlinks?: boolean }): Promise<T.StatResult> {
		const absolutePath = this.#resolvePath(filePath);
		opts?.signal?.throwIfAborted();
		try {
			const linkStat = await fs.lstat(absolutePath);
			const wasSymlink = linkStat.isSymbolicLink();
			const targetStat = wasSymlink && !opts?.followSymlinks ? linkStat : await fs.stat(absolutePath);
			const kind =
				wasSymlink && !opts?.followSymlinks
					? "symlink"
					: targetStat.isFile()
						? "file"
						: targetStat.isDirectory()
							? "dir"
							: "other";
			return {
				exists: true,
				kind,
				size: targetStat.size,
				mtimeMs: targetStat.mtimeMs,
				linkKind: wasSymlink ? "symlink" : undefined,
				etag: targetStat.isFile() ? hashBytes(await Bun.file(absolutePath).bytes()) : null,
			};
		} catch (error) {
			if (isEnoent(error)) {
				return { exists: false, kind: "other", size: 0, mtimeMs: 0, etag: null };
			}
			throw error;
		}
	}

	async exists(filePath: string, opts?: { signal?: AbortSignal }): Promise<boolean> {
		const absolutePath = this.#resolvePath(filePath);
		opts?.signal?.throwIfAborted();
		try {
			await fs.lstat(absolutePath);
			return true;
		} catch (error) {
			if (isEnoent(error)) return false;
			throw error;
		}
	}

	async delete(filePath: string, opts?: { signal?: AbortSignal }): Promise<void> {
		const absolutePath = this.#resolvePath(filePath);
		opts?.signal?.throwIfAborted();
		try {
			await fs.rm(absolutePath, { recursive: true, force: false });
		} catch (error) {
			throw toLocalFsError(error, filePath);
		}
	}

	async mkdir(filePath: string, opts?: { recursive?: boolean; signal?: AbortSignal }): Promise<void> {
		const absolutePath = this.#resolvePath(filePath);
		opts?.signal?.throwIfAborted();
		try {
			await fs.mkdir(absolutePath, { recursive: opts?.recursive ?? false });
		} catch (error) {
			throw toLocalFsError(error, filePath);
		}
	}

	async rename(from: string, to: string, opts?: { overwrite?: boolean; signal?: AbortSignal }): Promise<void> {
		const absoluteFrom = this.#resolvePath(from);
		const absoluteTo = this.#resolvePath(to);
		opts?.signal?.throwIfAborted();
		if (absoluteFrom === absoluteTo) {
			return;
		}
		if (!(opts?.overwrite ?? false)) {
			try {
				await fs.lstat(absoluteTo);
				throw new BadRequestError(400, {
					code: "bad-request",
					message: `Path already exists: ${to}`,
				});
			} catch (error) {
				if (!isEnoent(error)) {
					throw error;
				}
			}
		}
		try {
			await fs.rename(absoluteFrom, absoluteTo);
		} catch (error) {
			throw toLocalFsError(error, from);
		}
	}

	async archiveEntries(filePath: string, opts?: T.ArchiveEntriesOptions): Promise<T.ArchiveEntriesResult> {
		const absolutePath = this.#resolvePath(filePath);
		const format = detectArchiveFormat(absolutePath);
		const prefix = normalizeArchiveLookupPath(opts?.prefix);
		opts?.signal?.throwIfAborted();
		if (prefix === undefined) {
			throw new ToolError("Archive path cannot contain '..'");
		}
		const archive = await this.#readArchiveCacheEntry(absolutePath, format, opts?.signal);
		const filtered = archive.entries.filter(
			entry => !prefix || entry.path === prefix || entry.path.startsWith(`${prefix}/`),
		);
		const limit = opts?.limit ?? Number.MAX_SAFE_INTEGER;
		return {
			entries: filtered.slice(0, limit),
			format: archive.format,
			truncated: filtered.length > limit,
		};
	}

	async openArchive(filePath: string, opts?: { signal?: AbortSignal }): Promise<T.ArchiveSnapshot> {
		const absolutePath = this.#resolvePath(filePath);
		const format = detectArchiveFormat(absolutePath);
		opts?.signal?.throwIfAborted();
		const archive = await this.#readArchiveCacheEntry(absolutePath, format, opts?.signal, { retain: true });
		let closed = false;
		const ensureOpen = (): void => {
			if (closed) {
				throw new ToolError("Archive snapshot is closed");
			}
		};
		const close = async (): Promise<void> => {
			if (closed) return;
			closed = true;
			releaseArchiveCache(archive);
		};
		return {
			format: archive.format,
			async entries(snapshotOpts?: { signal?: AbortSignal }): Promise<T.ArchiveEntry[]> {
				ensureOpen();
				snapshotOpts?.signal?.throwIfAborted();
				return archive.entries.map(entry => ({ ...entry }));
			},
			async readEntry(name: string, snapshotOpts?: { signal?: AbortSignal }): Promise<Uint8Array> {
				ensureOpen();
				snapshotOpts?.signal?.throwIfAborted();
				const normalizedEntry = normalizeArchiveWritePath(name);
				const file = archive.files.get(normalizedEntry);
				if (!file) {
					throw new ToolError(`Archive file '${normalizedEntry}' not found`);
				}
				return file instanceof Uint8Array ? file : await file.bytes();
			},
			close,
			async [Symbol.asyncDispose](): Promise<void> {
				await close();
			},
		};
	}

	async archiveReadEntry(filePath: string, entry: string, opts?: { signal?: AbortSignal }): Promise<T.ReadBlobResult> {
		const absolutePath = this.#resolvePath(filePath);
		const format = detectArchiveFormat(absolutePath);
		const normalizedEntry = normalizeArchiveWritePath(entry);
		opts?.signal?.throwIfAborted();
		const archive = await this.#readArchiveCacheEntry(absolutePath, format, opts?.signal);
		const file = archive.files.get(normalizedEntry);
		if (!file) {
			throw new ToolError(`Archive file '${normalizedEntry}' not found`);
		}
		const bytes = file instanceof Uint8Array ? file : await file.bytes();
		return {
			bytes,
			size: bytes.byteLength,
			etag: hashBytes(bytes),
			contentType: undefined,
		};
	}

	async archiveWriteEntry(
		filePath: string,
		entry: string,
		bytes: Uint8Array,
		opts: { ifMatch?: string | "*"; signal?: AbortSignal },
	): Promise<T.WriteResult> {
		return this.archiveBulkWrite(filePath, [{ name: entry, bytes }], opts);
	}

	async archiveBulkWrite(
		filePath: string,
		entries: Array<{ name: string; bytes: Uint8Array }>,
		opts?: { ifMatch?: string | "*"; signal?: AbortSignal },
	): Promise<T.WriteResult> {
		const absolutePath = this.#resolvePath(filePath);
		const format = detectArchiveFormat(absolutePath);
		opts?.signal?.throwIfAborted();
		await fs.mkdir(path.dirname(absolutePath), { recursive: true });
		if (opts?.ifMatch !== undefined && opts.ifMatch !== "*") {
			const { etag } = await this.#readBytes(absolutePath);
			if (etag !== opts.ifMatch) {
				throw new EtagMismatchError(412, { code: "etag-mismatch", message: "ETag mismatch" });
			}
		}
		const normalizedEntries = entries.map(entry => ({
			name: normalizeArchiveWritePath(entry.name),
			bytes: entry.bytes,
		}));
		if (format === "zip") {
			const zipEntries: Record<string, Uint8Array> = {};
			if (await this.exists(absolutePath, { signal: opts?.signal })) {
				const existing = await this.#readArchiveFileMap(absolutePath, format, opts?.signal);
				for (const [entryPath, fileBytes] of existing) {
					zipEntries[entryPath] =
						fileBytes instanceof Uint8Array ? fileBytes : new Uint8Array(await fileBytes.bytes());
				}
			}
			for (const entry of normalizedEntries) {
				zipEntries[entry.name] = entry.bytes;
			}
			const { zipSync } = await loadFflate();
			const zipBytes = zipSync(zipEntries);
			await this.#writeFileAtomically(absolutePath, zipBytes);
			invalidateArchiveCache(absolutePath);
			return {
				etag: hashBytes(zipBytes),
				written: normalizedEntries.reduce((sum, item) => sum + item.bytes.byteLength, 0),
			};
		}
		const archiveEntries: Record<string, string | File | Uint8Array> = {};
		if (await this.exists(absolutePath, { signal: opts?.signal })) {
			const existing = await this.#readArchiveFileMap(absolutePath, format, opts?.signal);
			for (const [entryPath, file] of existing) {
				archiveEntries[entryPath] = file;
			}
		}
		for (const entry of normalizedEntries) {
			archiveEntries[entry.name] = entry.bytes;
		}
		const tmpPath = `${absolutePath}.tmp-${crypto.randomUUID()}`;
		await Bun.Archive.write(tmpPath, archiveEntries, format === "tar.gz" ? { compress: "gzip" } : undefined);
		await fs.rename(tmpPath, absolutePath);
		invalidateArchiveCache(absolutePath);
		const archiveBytes = await Bun.file(absolutePath).bytes();
		return {
			etag: hashBytes(archiveBytes),
			written: normalizedEntries.reduce((sum, item) => sum + item.bytes.byteLength, 0),
		};
	}
	async readAst(filePath: string, opts?: T.ReadAstOptions): Promise<T.AstSummary> {
		const absolutePath = this.#resolvePath(filePath);
		const text = await Bun.file(absolutePath).text();
		const normalized = normalizeNewlines(text.startsWith("\uFEFF") ? text.slice(1) : text);
		const slicedText = opts?.range ? applyLineRange(normalized.split("\n"), opts.range).lines.join("\n") : normalized;
		const summary = summarizeCode({
			code: slicedText,
			lang: opts?.language,
			path: absolutePath,
			minBodyLines: opts?.minBodyLines,
			minCommentLines: opts?.minCommentLines,
		});
		return {
			language: summary.language ?? null,
			parsed: summary.parsed,
			elided: summary.elided,
			total_lines: summary.totalLines,
			segments: summary.segments.map(segment => ({
				kind: segment.kind,
				start_line: segment.startLine,
				end_line: segment.endLine,
				text: segment.text ?? null,
			})),
		};
	}

	async imageMeta(filePath: string, opts?: { signal?: AbortSignal }): Promise<T.ImageMetadata | null> {
		opts?.signal?.throwIfAborted();
		return await readImageMetadata(this.#resolvePath(filePath));
	}

	async notebookText(filePath: string, displayPath: string, opts?: { signal?: AbortSignal }): Promise<string> {
		const blob = await this.readBlob(filePath, { signal: opts?.signal });
		return notebookToEditableText(new TextDecoder().decode(blob.bytes), displayPath);
	}

	async markitConvert(
		filePath: string,
		extension: string,
		opts?: { signal?: AbortSignal },
	): Promise<T.MarkitConversionResult> {
		const blob = await this.readBlob(filePath, { signal: opts?.signal });
		return await convertBufferWithMarkit(blob.bytes, extension, opts?.signal);
	}

	async listWorkspace(opts: T.ListWorkspaceOptions): Promise<T.ListWorkspaceResult> {
		return await nativeListWorkspace({
			path: this.#resolvePath(opts.path),
			maxDepth: opts.maxDepth,
			hidden: opts.hidden,
			gitignore: opts.gitignore,
			collectAgentsMd: opts.collectAgentsMd,
			timeoutMs: opts.timeoutMs,
			signal: opts.signal,
		});
	}

	async writeLines(filePath: string, text: string, opts: T.WriteOptions): Promise<T.WriteResult> {
		const absolutePath = this.#resolvePath(filePath);
		await fs.mkdir(path.dirname(absolutePath), { recursive: true });
		let targetEol: "LF" | "CRLF" | "CR" = detectEol(text);
		let targetBom = text.startsWith("\uFEFF");
		if (opts.ifMatch !== undefined && opts.ifMatch !== "*") {
			const { etag } = await this.#readBytes(absolutePath);
			if (etag !== opts.ifMatch) {
				throw new EtagMismatchError(412, { code: "etag-mismatch", message: "ETag mismatch" });
			}
		}
		try {
			const existing = await Bun.file(absolutePath).text();
			targetEol = detectEol(existing);
			targetBom = existing.startsWith("\uFEFF");
		} catch {}
		const sanitized = normalizeNewlines(text.startsWith("\uFEFF") ? text.slice(1) : text).replaceAll(
			"\n",
			eolString(targetEol),
		);
		const finalText = `${targetBom ? "\uFEFF" : ""}${sanitized}`;
		const bytes = textEncoder.encode(finalText);
		await this.#writeFileAtomically(absolutePath, bytes);
		return { etag: hashBytes(bytes), written: bytes.byteLength };
	}

	async writeBlob(filePath: string, bytes: Uint8Array, opts: T.WriteOptions): Promise<T.WriteResult> {
		const absolutePath = this.#resolvePath(filePath);
		await fs.mkdir(path.dirname(absolutePath), { recursive: true });
		if (opts.ifMatch !== undefined && opts.ifMatch !== "*") {
			const { etag } = await this.#readBytes(absolutePath);
			if (etag !== opts.ifMatch) {
				throw new EtagMismatchError(412, { code: "etag-mismatch", message: "ETag mismatch" });
			}
		}
		await this.#writeFileAtomically(absolutePath, bytes);
		return { etag: hashBytes(bytes), written: bytes.byteLength };
	}

	async glob(req: T.GlobRequest): Promise<T.GlobResult> {
		const requestedTypes = req.types ?? [];
		const roots = req.paths && req.paths.length > 0 ? req.paths : ["."];
		const limit = req.limit ?? Number.MAX_SAFE_INTEGER;
		const allEntries = new Map<
			string,
			{ path: string; type: "file" | "dir" | "symlink" | "other"; size: number; modified: number }
		>();
		let truncated = false;
		await Promise.all(
			roots.flatMap(root =>
				req.patterns.map(async pattern => {
					const absoluteRoot = this.#resolvePath(root);
					const relativeRoot = path.relative(this.cwd, absoluteRoot).replaceAll("\\", "/") || ".";
					const result = await nativeGlob(
						{
							pattern,
							path: absoluteRoot,
							hidden: req.includeHidden,
							maxResults: limit,
							recursive: true,
							sortByMtime: true,
							gitignore: req.gitignore,
							signal: req.signal,
							fileType: requestedTypes.length === 1 ? mapRequestedFileType(requestedTypes[0]) : undefined,
						},
						(_error, match) => {
							const entry = {
								path: withPrefixedPath(relativeRoot, match.path),
								type: mapFileType(match.fileType),
								size: match.size ?? 0,
								modified: match.mtime ?? 0,
							};
							if (req.maxDepth !== undefined && depthOfRelativePath(entry.path) > req.maxDepth) {
								return;
							}
							if (
								requestedTypes.length > 0 &&
								!requestedTypes.includes(entry.type as "file" | "dir" | "symlink")
							) {
								return;
							}
							if (allEntries.size >= limit) {
								truncated = true;
								return;
							}
							allEntries.set(entry.path, entry);
						},
					);
					if (result.totalMatches > result.matches.length || allEntries.size >= limit) {
						truncated = true;
					}
				}),
			),
		);
		return {
			entries: Array.from(allEntries.values())
				.sort((left, right) => right.modified - left.modified)
				.slice(0, limit),
			truncated,
		};
	}

	async *grep(req: T.GrepRequest): AsyncGenerator<T.GrepHit, T.GrepSummary> {
		const queue = new AsyncQueue<T.GrepHit>();
		let limitReached = false;
		let truncated = false;
		void (async () => {
			try {
				for (const root of req.paths) {
					const absolutePath = this.#resolvePath(root);
					const relativeRoot = path.relative(this.cwd, absolutePath).replaceAll("\\", "/") || ".";
					const result = await nativeGrep({
						pattern: req.pattern,
						path: absolutePath,
						ignoreCase: req.ignoreCase,
						multiline: req.multiline,
						gitignore: req.gitignore,
						contextBefore: req.contextBefore ?? req.contextLines,
						contextAfter: req.contextAfter ?? req.contextLines,
						maxCount: req.maxMatches,
						signal: req.signal,
					});
					for (const match of result.matches) {
						for (const context of match.contextBefore ?? []) {
							queue.push({
								path: withPrefixedPath(relativeRoot, match.path),
								line: context.lineNumber,
								kind: "context",
								text: context.line,
							});
						}
						if (match.truncated) {
							truncated = true;
						}
						queue.push({
							path: withPrefixedPath(relativeRoot, match.path),
							line: match.lineNumber,
							kind: "match",
							text: match.line,
							truncated: match.truncated,
						});
						for (const context of match.contextAfter ?? []) {
							queue.push({
								path: withPrefixedPath(relativeRoot, match.path),
								line: context.lineNumber,
								kind: "context",
								text: context.line,
							});
						}
					}
					limitReached ||= result.limitReached === true;
				}
				queue.finish();
			} catch (error) {
				queue.finish(error);
			}
		})();
		while (true) {
			const next = await queue.next();
			if (next.done) {
				return { type: "summary", limitReached, truncated };
			}
			yield next.value;
		}
	}
}

class LocalEditBackend implements EditBackend {
	readonly #pipeline: BackendEditPipeline;

	constructor(
		fs: LocalFsBackend,
		private readonly cwd: string,
	) {
		this.#pipeline = new BackendEditPipeline(fs);
	}

	#resolvePath(filePath: string): string {
		return path.isAbsolute(filePath) ? filePath : path.resolve(this.cwd, filePath);
	}

	async patch(req: T.EditPatchRequest): Promise<T.EditResult> {
		return await this.#pipeline.patch(req);
	}

	async replace(req: T.EditReplaceRequest): Promise<T.EditResult> {
		return await this.#pipeline.replace(req);
	}

	async editAst(req: T.EditAstRequest): Promise<T.AstEditResult> {
		const ops = getAstEditOps(req);
		const rewriteMap = Object.fromEntries(ops.map(op => [op.pat, op.out]));
		const preview = await astEdit({
			rewrites: rewriteMap,
			lang: req.language,
			path: this.cwd,
			glob: req.paths.length === 1 ? req.paths[0] : `{${req.paths.join(",")}}`,
			dryRun: true,
			signal: req.signal,
		});
		const originals = new Map<string, string>();
		await Promise.all(
			preview.fileChanges.map(async change => {
				const absolutePath = this.#resolvePath(change.path);
				originals.set(change.path, await Bun.file(absolutePath).text());
			}),
		);
		if (req.dryRun) {
			return {
				changes: preview.fileChanges.map(change => ({
					path: change.path,
					replacements: change.count,
					diff: buildAstEditDiff(preview.changes, change.path),
				})),
				fileChanges: buildAstStructuredFileChanges(preview.fileChanges, preview.changes, originals),
				parseErrors: parseAstParseErrors(preview.parseErrors),
				filesSearched: preview.filesSearched,
				limitReached: preview.limitReached,
				written: preview.applied,
				truncated: preview.limitReached,
				exceededLimit: preview.limitReached,
			};
		}
		const applied = await astEdit({
			rewrites: rewriteMap,
			lang: req.language,
			path: this.cwd,
			glob: req.paths.length === 1 ? req.paths[0] : `{${req.paths.join(",")}}`,
			dryRun: false,
			signal: req.signal,
		});
		const nextTexts = new Map<string, string>();
		await Promise.all(
			applied.fileChanges.map(async change => {
				nextTexts.set(change.path, await Bun.file(this.#resolvePath(change.path)).text());
			}),
		);
		return {
			changes: applied.fileChanges.map(change => ({
				path: change.path,
				replacements: change.count,
				diff: generateDiff(originals.get(change.path) ?? "", nextTexts.get(change.path) ?? ""),
			})),
			fileChanges: buildAstStructuredFileChanges(applied.fileChanges, applied.changes, originals, nextTexts),
			parseErrors: parseAstParseErrors(applied.parseErrors),
			filesSearched: applied.filesSearched,
			limitReached: applied.limitReached,
			written: applied.applied,
			truncated: applied.limitReached,
			exceededLimit: applied.limitReached,
		};
	}

	async *grepAst(req: T.GrepAstRequest): AsyncGenerator<T.GrepAstHit, T.GrepAstSummary> {
		const result = await astGrep({
			patterns: [req.pattern],
			path: this.cwd,
			glob: req.paths.length === 1 ? req.paths[0] : `{${req.paths.join(",")}}`,
			lang: req.language,
			strictness: req.strictness ? AST_MATCH_STRICTNESS[req.strictness] : undefined,
			limit: req.limit,
			includeMeta: true,
			signal: req.signal,
		});
		for (const match of result.matches) {
			yield mapAstMatch(match);
		}
		return {
			type: "summary",
			parseErrors: parseAstParseErrors(result.parseErrors),
			filesSearched: result.filesSearched,
			limitReached: result.limitReached,
		};
	}
}

function parseAstParseErrors(errors: string[] | undefined): T.GrepAstParseError[] {
	return (errors ?? []).map(error => {
		const [file, ...rest] = error.split(": ");
		return rest.length > 0 ? { file, message: rest.join(": ") } : { message: error };
	});
}

function mapAstMatch(match: {
	path: string;
	startLine: number;
	startColumn: number;
	endLine: number;
	endColumn: number;
	text: string;
	metaVariables?: Record<string, string>;
}): T.GrepAstHit {
	return {
		file: match.path,
		range: { start: match.startLine, end: match.endLine },
		column: match.startColumn,
		endColumn: match.endColumn,
		matched: match.text,
		meta: match.metaVariables ?? {},
	};
}

function generateDiff(before: string, after: string): string {
	const beforeLines = normalizeNewlines(before).split("\n");
	const afterLines = normalizeNewlines(after).split("\n");
	const max = Math.max(beforeLines.length, afterLines.length);
	const output: string[] = [];
	for (let index = 0; index < max; index += 1) {
		const left = beforeLines[index];
		const right = afterLines[index];
		if (left === right) continue;
		if (left !== undefined) output.push(`-${index + 1}|${left}`);
		if (right !== undefined) output.push(`+${index + 1}|${right}`);
	}
	return output.join("\n");
}

class LocalShellBackend implements ShellBackend {
	readonly #shells = new Map<string, Shell>();

	exec(req: T.BashExecRequest): AsyncIterable<T.BashEvent> {
		const queue = new AsyncQueue<T.BashEvent>();
		const sessionKey = req.sessionKey ?? "default";
		let shell = this.#shells.get(sessionKey);
		if (!shell) {
			shell = new Shell();
			this.#shells.set(sessionKey, shell);
		}
		const runOptions = {
			command: req.command,
			cwd: req.cwd ?? undefined,
			env: req.env ?? undefined,
			timeoutMs: req.timeout_ms ?? undefined,
			minimizer: req.minimizer
				? {
						enabled: req.minimizer.enabled,
					}
				: undefined,
			signal: req.signal,
		};
		void shell
			.run(runOptions, (_error, chunk) => {
				queue.push({ type: "output", text: chunk });
			})
			.then(async result => {
				let rawArtifact: { kind: "path"; path: string } | undefined;
				if (result.minimized) {
					const artifactPath = path.join(process.env.TMPDIR ?? "/tmp", `omp-bash-${crypto.randomUUID()}.log`);
					await fs.writeFile(artifactPath, result.minimized.originalText);
					rawArtifact = { kind: "path", path: artifactPath };
				}
				queue.push({
					type: "exit",
					exitCode: result.exitCode ?? null,
					signaled: result.cancelled || result.timedOut,
					cancelled: result.cancelled,
					timedOut: result.timedOut,
					minimizer: result.minimized
						? {
								minimized: true,
								originalLines: result.minimized.originalText.split("\n").length,
								minimizedLines: result.minimized.text.split("\n").length,
								omittedLines:
									result.minimized.originalText.split("\n").length - result.minimized.text.split("\n").length,
								truncated: result.minimized.outputBytes < result.minimized.inputBytes,
								rawArtifact,
							}
						: undefined,
				});
				queue.finish();
			})
			.catch(error => queue.finish(error));
		return queue;
	}

	async dispose(): Promise<void> {
		this.#shells.clear();
	}
}

class LocalSqliteBackend implements SqliteBackend {
	async read(req: T.ReadDbRequest): Promise<T.ReadDbResponse> {
		const db = new Database(req.path, { readonly: true });
		try {
			if (req.q) {
				const result = executeReadQuery(db, req.q);
				return makeDbResponse(
					result.rows,
					result.columns.map(name => ({ name, type: "" })),
				);
			}
			if (!req.table) {
				return {
					tables: listTables(db).map(table => ({
						name: table.name,
						row_count: table.count.rows,
						columns: makeDbColumns(db, table.name),
					})),
				};
			}
			if (req.key) {
				const lookup = resolveTableRowLookup(db, req.table);
				const row =
					lookup.kind === "pk"
						? getRowByKey(db, req.table, lookup, req.key)
						: getRowByRowId(db, req.table, req.key);
				return makeDbResponse(
					row ? [row] : [],
					makeDbColumns(db, req.table),
					lookup.kind === "rowid" ? "rowid" : lookup.column,
				);
			}
			const result = queryRows(db, req.table, {
				where: req.where ?? undefined,
				order: req.order ?? undefined,
				limit: req.limit ?? 50,
				offset: req.offset ?? 0,
			});
			return makeDbResponse(
				result.rows,
				result.columns.map(name => ({ name, type: "" })),
			);
		} finally {
			db.close();
		}
	}

	async write(req: T.WriteDbRequest): Promise<T.WriteDbResponse> {
		const db = new Database(req.path);
		try {
			let affected = 0;
			if (req.op === "exec") {
				if (!req.sql) {
					throw new ToolError("SQLite exec requires sql");
				}
				affected = db.run(req.sql).changes;
			} else {
				if (!req.table) {
					throw new ToolError(`SQLite ${req.op} requires table`);
				}
				const lookup = req.key ? resolveTableRowLookup(db, req.table) : null;
				switch (req.op) {
					case "insert":
						insertRow(db, req.table, req.row ?? {});
						affected = 1;
						break;
					case "update":
						if (!req.key || !lookup) throw new ToolError("SQLite update requires key");
						affected =
							lookup.kind === "pk"
								? updateRowByKey(db, req.table, lookup, req.key, req.row ?? {})
								: updateRowByRowId(db, req.table, req.key, req.row ?? {});
						break;
					case "delete":
						if (!req.key || !lookup) throw new ToolError("SQLite delete requires key");
						affected =
							lookup.kind === "pk"
								? deleteRowByKey(db, req.table, lookup, req.key)
								: deleteRowByRowId(db, req.table, req.key);
						break;
				}
			}
			return { affected };
		} finally {
			db.close();
		}
	}
}

class LocalKernelBackend implements KernelBackend {
	readonly #kernels = new Map<string, T.KernelConfig>();
	readonly #session: ToolSession;

	constructor(cwd: string, backend: Backend) {
		this.#session = makeToolSession(cwd, backend);
	}

	async put(name: string, cfg: T.KernelConfig): Promise<T.KernelStatus> {
		this.#kernels.set(name, cfg);
		return { name, state: "ready", config: cfg };
	}

	async get(name: string): Promise<T.KernelStatus | null> {
		const config = this.#kernels.get(name);
		return config ? { name, state: "ready", config } : null;
	}

	async *exec(name: string, req: T.KernelExecRequest): AsyncIterable<T.KernelEvent> {
		const config = this.#kernels.get(name);
		if (!config) {
			throw new Error(`Unknown kernel ${name}`);
		}
		if (config.lang === "python") {
			const result = await executePython(req.code, {
				sessionId: name,
				kernelOwnerId: name,
				cwd: req.cwd ?? process.cwd(),
				timeoutMs: req.timeout_ms ?? undefined,
				signal: req.signal,
			});
			if (result.output) {
				yield { type: "stdout", data: result.output };
			}
			for (const display of result.displayOutputs) {
				if (display.type === "image") {
					yield { type: "display", mime: display.mimeType, data: display.data };
					continue;
				}
				if (display.type === "json") {
					yield { type: "display", mime: "application/json", data: JSON.stringify(display.data) };
					continue;
				}
				if (display.type === "markdown") {
					yield { type: "display", mime: "text/markdown", data: "" };
					continue;
				}
				yield { type: "display", mime: "application/x-omp-status", data: JSON.stringify(display.event) };
			}
			yield { type: "status", state: mapKernelState(result) === "error" ? "busy" : "idle" };
			return;
		}
		const result = await executeJs(req.code, {
			sessionId: name,
			session: this.#session,
			cwd: req.cwd ?? this.#session.cwd,
			deadlineMs: Date.now() + (req.timeout_ms ?? 30_000),
			reset: false,
			signal: req.signal,
		});
		if (result.output) {
			yield { type: result.exitCode === 1 ? "stderr" : "stdout", data: result.output };
		}
		for (const display of result.displayOutputs) {
			if (display.type === "image") {
				yield { type: "display", mime: display.mimeType, data: display.data };
				continue;
			}
			if (display.type === "json") {
				yield { type: "display", mime: "application/json", data: JSON.stringify(display.data) };
				continue;
			}
			yield { type: "display", mime: "application/x-omp-status", data: JSON.stringify(display.event) };
		}
		yield { type: "status", state: result.exitCode === 1 ? "busy" : "idle" };
	}

	async delete(name: string): Promise<void> {
		const config = this.#kernels.get(name);
		if (!config) return;
		this.#kernels.delete(name);
		if (config.lang === "python") {
			await disposeKernelSessionsByOwner(name);
			return;
		}
		await resetVmContext(name);
	}

	async dispose(): Promise<void> {
		const names = Array.from(this.#kernels.keys());
		await Promise.allSettled(names.map(async name => await this.delete(name)));
	}
}

interface LocalLspEntry {
	config: T.LspConfig;
	cwd: string;
	client?: LspClient;
}

class LocalLspBackend implements LspBackend {
	readonly #entries = new Map<string, LocalLspEntry>();

	constructor(private readonly cwd: string) {}

	async put(name: string, cfg: T.LspConfig): Promise<T.LspStatus> {
		const entry: LocalLspEntry = {
			config: cfg,
			cwd: fileUriToPath(cfg.root_uri) ?? this.cwd,
		};
		entry.client = await getOrCreateClient(toServerConfig(cfg), entry.cwd, cfg.idle_timeout_ms ?? undefined);
		this.#entries.set(name, entry);
		return localLspStatus(name, cfg, entry.client);
	}

	async get(name: string): Promise<T.LspStatus | null> {
		const entry = this.#entries.get(name);
		return entry?.client
			? localLspStatus(name, entry.config, entry.client)
			: entry
				? { name, state: "starting", config: entry.config }
				: null;
	}

	async openChannel(name: string): Promise<JsonRpcChannel> {
		const entry = this.#entries.get(name);
		if (!entry) throw new Error(`Unknown LSP handle ${name}`);
		const client =
			entry.client ??
			(await getOrCreateClient(toServerConfig(entry.config), entry.cwd, entry.config.idle_timeout_ms ?? undefined));
		entry.client = client;
		const handlers = new Set<(method: string, params: unknown) => void>();
		const close = async (): Promise<void> => {
			handlers.clear();
		};
		return {
			async request<TResp = unknown>(
				method: string,
				params?: unknown,
				opts?: { signal?: AbortSignal; timeoutMs?: number },
			): Promise<TResp> {
				return (await sendLspRequest(client, method, params ?? null, opts?.signal, opts?.timeoutMs)) as TResp;
			},
			notify(method: string, params?: unknown): void {
				void sendLspNotification(client, method, params ?? null);
			},
			onNotification(handler: (method: string, params: unknown) => void): () => void {
				handlers.add(handler);
				return () => {
					handlers.delete(handler);
				};
			},
			setReverseRequestHandler(): () => void {
				return () => undefined;
			},
			close,
			async [Symbol.asyncDispose](): Promise<void> {
				await close();
			},
		};
	}

	async delete(name: string): Promise<void> {
		const entry = this.#entries.get(name);
		if (!entry) return;
		this.#entries.delete(name);
		if (entry.client) {
			await shutdownClientInstance(entry.client);
		}
	}

	async dispose(): Promise<void> {
		const names = Array.from(this.#entries.keys());
		await Promise.allSettled(names.map(async name => await this.delete(name)));
	}
}

interface LocalDapEntry {
	config: T.DapConfig;
	client?: DapClient;
}

class LocalDapBackend implements DapBackend {
	readonly #entries = new Map<string, LocalDapEntry>();

	constructor(private readonly cwd: string) {}

	async put(name: string, cfg: T.DapConfig, opts?: { signal?: AbortSignal }): Promise<T.DapStatus> {
		const entry: LocalDapEntry = { config: cfg };
		entry.client = await untilAborted(
			opts?.signal,
			async () => await DapClient.spawn({ adapter: toResolvedAdapter(cfg), cwd: this.cwd }),
		);
		this.#entries.set(name, entry);
		return { name, state: "ready", config: cfg };
	}

	async get(name: string): Promise<T.DapStatus | null> {
		const entry = this.#entries.get(name);
		return entry ? { name, state: "ready", config: entry.config } : null;
	}

	async openChannel(name: string): Promise<JsonRpcChannel> {
		const entry = this.#entries.get(name);
		if (!entry) throw new Error(`Unknown DAP handle ${name}`);
		const client =
			entry.client ?? (await DapClient.spawn({ adapter: toResolvedAdapter(entry.config), cwd: this.cwd }));
		entry.client = client;
		const unsubscribers = new Set<() => void>();
		const close = async (): Promise<void> => {
			for (const unsubscribe of unsubscribers) unsubscribe();
			unsubscribers.clear();
		};
		return {
			async request<TResp = unknown>(
				method: string,
				params?: unknown,
				opts?: { signal?: AbortSignal; timeoutMs?: number },
			): Promise<TResp> {
				return await client.sendRequest<TResp>(method, params, opts?.signal, opts?.timeoutMs);
			},
			notify(method: string, params?: unknown): void {
				void client.sendRequest(method, params).catch(() => undefined);
			},
			onNotification(handler: (method: string, params: unknown) => void): () => void {
				const unsubscribe = client.onAnyEvent((body, event) => {
					handler(event.event, body);
				});
				unsubscribers.add(unsubscribe);
				return () => {
					unsubscribe();
					unsubscribers.delete(unsubscribe);
				};
			},
			setReverseRequestHandler(method: string, handler: (params: unknown) => Promise<unknown>): () => void {
				const unsubscribe = client.onReverseRequest(method, handler);
				unsubscribers.add(unsubscribe);
				return () => {
					unsubscribe();
					unsubscribers.delete(unsubscribe);
				};
			},
			close,
			async [Symbol.asyncDispose](): Promise<void> {
				await close();
			},
		};
	}

	async delete(name: string): Promise<void> {
		const entry = this.#entries.get(name);
		if (!entry) return;
		this.#entries.delete(name);
		await entry.client?.dispose();
	}

	async dispose(): Promise<void> {
		const names = Array.from(this.#entries.keys());
		await Promise.allSettled(names.map(async name => await this.delete(name)));
	}
}

interface LocalBrowserEntry {
	config: T.BrowserConfig;
	handle: BrowserHandle;
}

class LocalBrowserBackend implements BrowserBackend {
	readonly #entries = new Map<string, LocalBrowserEntry>();

	constructor(private readonly cwd: string) {}

	async put(name: string, cfg: T.BrowserConfig, opts?: { signal?: AbortSignal }): Promise<T.BrowserStatus> {
		opts?.signal?.throwIfAborted();
		const handle = await acquireBrowser(toBrowserKind(cfg), { cwd: this.cwd, signal: opts?.signal ?? cfg.signal });
		holdBrowser(handle);
		const existing = this.#entries.get(name);
		if (existing) {
			await releaseBrowser(existing.handle, { kill: true });
		}
		this.#entries.set(name, { config: cfg, handle });
		return { name, state: "ready", cdpUrl: browserCdpUrl(handle) };
	}

	async get(name: string, opts?: { signal?: AbortSignal }): Promise<T.BrowserStatus | null> {
		opts?.signal?.throwIfAborted();
		const entry = this.#entries.get(name);
		if (!entry) return null;
		return { name, state: "ready", cdpUrl: browserCdpUrl(entry.handle) };
	}

	async list(opts?: { signal?: AbortSignal }): Promise<T.BrowserStatus[]> {
		opts?.signal?.throwIfAborted();
		return Array.from(this.#entries.entries()).map(([name, entry]) => ({
			name,
			state: "ready",
			cdpUrl: browserCdpUrl(entry.handle),
		}));
	}

	async wsUrl(name: string, opts?: { signal?: AbortSignal }): Promise<string> {
		opts?.signal?.throwIfAborted();
		const entry = this.#entries.get(name);
		if (!entry) throw new Error(`Unknown browser handle ${name}`);
		return browserCdpUrl(entry.handle);
	}

	async delete(name: string, opts?: { signal?: AbortSignal }): Promise<void> {
		opts?.signal?.throwIfAborted();
		const entry = this.#entries.get(name);
		if (!entry) return;
		this.#entries.delete(name);
		await releaseBrowser(entry.handle, { kill: true });
	}

	async dispose(): Promise<void> {
		const names = Array.from(this.#entries.keys());
		await Promise.allSettled(names.map(async name => await this.delete(name)));
	}
}

function localLspStatus(name: string, config: T.LspConfig, client: LspClient): T.LspStatus {
	return {
		name,
		state: "ready",
		config,
		capabilities: client.serverCapabilities,
		projectLoaded: client.status === "ready",
		openFiles: Array.from(client.openFiles.keys()),
		diagnostics: new Map(
			Array.from(client.diagnostics.entries()).map(([uri, published]) => [uri, published.diagnostics]),
		),
	};
}

function toServerConfig(config: T.LspConfig): ServerConfig {
	return {
		command: config.command,
		args: config.args ?? [],
		fileTypes: ["*"],
		rootMarkers: [],
		resolvedCommand: config.command,
		initOptions: isRecord(config.initialization_options) ? config.initialization_options : undefined,
	};
}

function toResolvedAdapter(config: T.DapConfig): DapResolvedAdapter {
	return {
		name: path.basename(config.command),
		command: config.command,
		args: config.args ?? [],
		resolvedCommand: config.command,
		languages: [],
		fileTypes: [],
		rootMarkers: [],
		launchDefaults: {},
		attachDefaults: {},
		connectMode: config.transport === "tcp" ? "tcp" : "stdio",
		acceptsDirectoryProgram: false,
	};
}

function browserCdpUrl(handle: BrowserHandle): string {
	if ("browser" in handle) {
		return handle.cdpUrl ?? handle.browser.wsEndpoint();
	}
	throw new Error("Browser handle has no CDP URL");
}

function toBrowserKind(config: T.BrowserConfig): BrowserKind {
	if (config.kind === "cdp-attach") {
		return { kind: "connected", cdpUrl: config.cdp_url };
	}
	if (!config.path) {
		return { kind: "headless", headless: config.headless ?? true };
	}
	return {
		kind: "spawned",
		path: config.path,
		args: config.args,
	};
}

function fileUriToPath(uri: string | null | undefined): string | null {
	if (!uri?.startsWith("file://")) return null;
	try {
		return decodeURIComponent(new URL(uri).pathname);
	} catch {
		return null;
	}
}

function isRecord(value: unknown): value is Record<string, unknown> {
	return typeof value === "object" && value !== null && !Array.isArray(value);
}

export interface LocalBackendOptions {
	cwd: string;
}

export class LocalBackend implements Backend {
	readonly kind = "local" as const;
	readonly fs: FsBackend;
	readonly edit: EditBackend;
	readonly shell: ShellBackend;
	readonly sqlite: SqliteBackend;
	readonly kernel: KernelBackend;
	readonly lsp: LspBackend;
	readonly dap: DapBackend;
	readonly browser: BrowserBackend;
	readonly #shellBackend: LocalShellBackend;
	readonly #kernelBackend: LocalKernelBackend;
	readonly #lspBackend: LocalLspBackend;
	readonly #dapBackend: LocalDapBackend;
	readonly #browserBackend: LocalBrowserBackend;
	#disposed = false;

	constructor(options: LocalBackendOptions) {
		const fsBackend = new LocalFsBackend(options.cwd);
		const shellBackend = new LocalShellBackend();
		const lspBackend = new LocalLspBackend(options.cwd);
		const dapBackend = new LocalDapBackend(options.cwd);
		const browserBackend = new LocalBrowserBackend(options.cwd);
		this.fs = fsBackend;
		this.edit = new LocalEditBackend(fsBackend, options.cwd);
		this.shell = shellBackend;
		this.sqlite = new LocalSqliteBackend();
		this.lsp = lspBackend;
		this.dap = dapBackend;
		this.browser = browserBackend;
		const kernelBackend = new LocalKernelBackend(options.cwd, this);
		this.kernel = kernelBackend;
		this.#shellBackend = shellBackend;
		this.#kernelBackend = kernelBackend;
		this.#lspBackend = lspBackend;
		this.#dapBackend = dapBackend;
		this.#browserBackend = browserBackend;
	}

	async dispose(): Promise<void> {
		if (this.#disposed) return;
		this.#disposed = true;
		await Promise.allSettled([
			this.#browserBackend.dispose(),
			this.#dapBackend.dispose(),
			this.#lspBackend.dispose(),
			this.#kernelBackend.dispose(),
			this.#shellBackend.dispose(),
		]);
	}
}

import { NotFoundError } from "@oh-my-pi/rwp-client";
import type * as T from "./types";

function normalizeNewlines(text: string): string {
	return text.replaceAll("\r\n", "\n").replaceAll("\r", "\n");
}

function replaceLiteral(content: string, oldText: string, newText: string, replaceAll: boolean): string {
	if (replaceAll) {
		return content.split(oldText).join(newText);
	}
	return content.replace(oldText, newText);
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

function generateEditResult(before: string, after: string, existed: boolean): T.EditResult {
	let firstChangedLine: number | null = null;
	const beforeLines = normalizeNewlines(before).split("\n");
	const afterLines = normalizeNewlines(after).split("\n");
	const max = Math.max(beforeLines.length, afterLines.length);
	for (let index = 0; index < max; index += 1) {
		if (beforeLines[index] !== afterLines[index]) {
			firstChangedLine = index + 1;
			break;
		}
	}
	return {
		diff: generateDiff(before, after),
		first_changed_line: firstChangedLine,
		op: existed ? "update" : "create",
	};
}

function isMissingFileError(error: unknown): boolean {
	if (error instanceof NotFoundError) return true;
	return (error as NodeJS.ErrnoException | undefined)?.code === "ENOENT";
}

export interface FsBackend {
	readLines(path: string, opts?: T.ReadLinesOptions): Promise<T.ReadLinesResult>;
	readBlob(path: string, opts?: T.ReadBlobOptions): Promise<T.ReadBlobResult>;
	readAst(path: string, opts?: T.ReadAstOptions): Promise<T.AstSummary>;
	imageMeta(path: string, opts?: { signal?: AbortSignal }): Promise<T.ImageMetadata | null>;
	notebookText(path: string, displayPath: string, opts?: { signal?: AbortSignal }): Promise<string>;
	markitConvert(path: string, extension: string, opts?: { signal?: AbortSignal }): Promise<T.MarkitConversionResult>;
	listWorkspace(opts: T.ListWorkspaceOptions): Promise<T.ListWorkspaceResult>;
	writeLines(path: string, text: string, opts: T.WriteOptions): Promise<T.WriteResult>;
	writeBlob(path: string, bytes: Uint8Array, opts: T.WriteOptions): Promise<T.WriteResult>;
	stat(path: string, opts?: { signal?: AbortSignal; followSymlinks?: boolean }): Promise<T.StatResult>;
	exists(path: string, opts?: { signal?: AbortSignal }): Promise<boolean>;
	delete(path: string, opts?: { signal?: AbortSignal }): Promise<void>;
	mkdir(path: string, opts?: { recursive?: boolean; signal?: AbortSignal }): Promise<void>;
	rename(from: string, to: string, opts?: { overwrite?: boolean; signal?: AbortSignal }): Promise<void>;
	archiveEntries(path: string, opts?: T.ArchiveEntriesOptions): Promise<T.ArchiveEntriesResult>;
	openArchive(path: string, opts?: { signal?: AbortSignal }): Promise<T.ArchiveSnapshot>;
	archiveReadEntry(path: string, entry: string, opts?: { signal?: AbortSignal }): Promise<T.ReadBlobResult>;
	archiveWriteEntry(
		path: string,
		entry: string,
		bytes: Uint8Array,
		opts: { ifMatch?: string | "*"; signal?: AbortSignal },
	): Promise<T.WriteResult>;
	archiveBulkWrite(
		path: string,
		entries: Array<{ name: string; bytes: Uint8Array }>,
		opts?: { ifMatch?: string | "*"; signal?: AbortSignal },
	): Promise<T.WriteResult>;
	glob(req: T.GlobRequest): Promise<T.GlobResult>;
	grep(req: T.GrepRequest): AsyncGenerator<T.GrepHit, T.GrepSummary>;
}

export interface EditBackend {
	patch(req: T.EditPatchRequest): Promise<T.EditResult>;
	replace(req: T.EditReplaceRequest): Promise<T.EditResult>;
	editAst(req: T.EditAstRequest): Promise<T.AstEditResult>;
	grepAst(req: T.GrepAstRequest): AsyncGenerator<T.GrepAstHit, T.GrepAstSummary>;
}

export class BackendEditPipeline {
	constructor(private readonly fs: FsBackend) {}

	async patch(req: T.EditPatchRequest): Promise<T.EditResult> {
		let existingText = "";
		let existingEtag: string | null = null;
		let existed = true;
		try {
			const read = await this.fs.readLines(req.path);
			existingText = read.lines.join("\n");
			existingEtag = read.etag;
		} catch (error) {
			if (!isMissingFileError(error)) throw error;
			existed = false;
		}

		const lines = normalizeNewlines(existingText).split("\n");
		let delta = 0;
		for (const hunk of req.hunks) {
			const startIndex = Math.max(0, hunk.start - 1 + delta);
			lines.splice(startIndex, hunk.deleted, ...hunk.inserted);
			delta += hunk.inserted.length - hunk.deleted;
		}
		const nextText = lines.join("\n");
		await this.fs.writeLines(req.path, nextText, { ifMatch: req.ifMatch ?? existingEtag ?? undefined });
		return generateEditResult(existingText, nextText, existed);
	}

	async replace(req: T.EditReplaceRequest): Promise<T.EditResult> {
		let existingText = "";
		let existingEtag: string | null = null;
		let existed = true;
		try {
			const read = await this.fs.readLines(req.path);
			existingText = read.lines.join("\n");
			existingEtag = read.etag;
		} catch (error) {
			if (!isMissingFileError(error)) throw error;
			existed = false;
		}

		const nextText = req.regex
			? existingText.replace(new RegExp(req.old, req.regexFlags ?? (req.all ? "g" : "")), req.new)
			: replaceLiteral(existingText, req.old, req.new, req.all ?? false);
		await this.fs.writeLines(req.path, nextText, { ifMatch: req.ifMatch ?? existingEtag ?? undefined });
		return generateEditResult(existingText, nextText, existed);
	}
}

export interface ShellBackend {
	exec(req: T.BashExecRequest): AsyncIterable<T.BashEvent>;
}

export interface SqliteBackend {
	read(req: T.ReadDbRequest): Promise<T.ReadDbResponse>;
	write(req: T.WriteDbRequest): Promise<T.WriteDbResponse>;
}

export interface KernelBackend {
	put(name: string, cfg: T.KernelConfig): Promise<T.KernelStatus>;
	get(name: string): Promise<T.KernelStatus | null>;
	exec(name: string, req: T.KernelExecRequest): AsyncIterable<T.KernelEvent>;
	delete(name: string): Promise<void>;
}

export interface JsonRpcChannel extends AsyncDisposable {
	request<TResp = unknown>(
		method: string,
		params?: unknown,
		opts?: { signal?: AbortSignal; timeoutMs?: number },
	): Promise<TResp>;
	notify(method: string, params?: unknown): void;
	onNotification(handler: (method: string, params: unknown) => void): () => void;
	setReverseRequestHandler(method: string, handler: (params: unknown) => Promise<unknown>): () => void;
	close(): Promise<void>;
}

export interface LspBackend {
	put(name: string, cfg: T.LspConfig): Promise<T.LspStatus>;
	get(name: string): Promise<T.LspStatus | null>;
	openChannel(name: string, opts?: { signal?: AbortSignal }): Promise<JsonRpcChannel>;
	delete(name: string): Promise<void>;
}

export interface DapBackend {
	put(name: string, cfg: T.DapConfig, opts?: { signal?: AbortSignal }): Promise<T.DapStatus>;
	get(name: string): Promise<T.DapStatus | null>;
	openChannel(name: string, opts?: { signal?: AbortSignal }): Promise<JsonRpcChannel>;
	delete(name: string): Promise<void>;
}

export interface BrowserBackend {
	put(name: string, cfg: T.BrowserConfig, opts?: { signal?: AbortSignal }): Promise<T.BrowserStatus>;
	get(name: string, opts?: { signal?: AbortSignal }): Promise<T.BrowserStatus | null>;
	list(opts?: { signal?: AbortSignal }): Promise<T.BrowserStatus[]>;
	wsUrl(name: string, opts?: { signal?: AbortSignal }): Promise<string>;
	delete(name: string, opts?: { signal?: AbortSignal }): Promise<void>;
}

export interface Backend {
	readonly kind: "local" | "remote";
	readonly fs: FsBackend;
	readonly edit: EditBackend;
	readonly shell: ShellBackend;
	readonly sqlite: SqliteBackend;
	readonly kernel: KernelBackend;
	readonly lsp: LspBackend;
	readonly dap: DapBackend;
	readonly browser: BrowserBackend;
	dispose(): Promise<void>;
}

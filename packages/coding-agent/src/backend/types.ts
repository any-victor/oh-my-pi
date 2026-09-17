import type { GlobMatch } from "@oh-my-pi/pi-natives";
import type { ImageMetadata } from "@oh-my-pi/pi-utils";
import type {
	AstOp,
	EvalEvent,
	EvalExecRequest,
	EvalHandleConfig,
	GrepRecord,
	ReadAstResponse,
	ReadDbQuery,
	AstEditFileChange as RwpAstEditFileChange,
	AstEditHunk as RwpAstEditHunk,
	AstFileChange as RwpAstFileChange,
	BashExecRequest as RwpBashExecRequest,
	CdpHandleConfig as RwpCdpHandleConfig,
	DapHandleConfig as RwpDapHandleConfig,
	EditAstRequest as RwpEditAstRequest,
	EditPatchRequest as RwpEditPatchRequest,
	EditReplaceRequest as RwpEditReplaceRequest,
	EditResult as RwpEditResult,
	LspHandleConfig as RwpLspHandleConfig,
	ReadDbResponse as RwpReadDbResponse,
	WriteDbRequest as RwpWriteDbRequest,
	WriteDbResponse as RwpWriteDbResponse,
	SqliteTableInfo,
} from "@oh-my-pi/rwp-client";
import type { Diagnostic, LspServerCapabilities } from "../lsp/types";
import type { MarkitConversionResult } from "../utils/markit";

export type BackendKind = "local" | "remote";

export interface ReadLinesOptions {
	range?: { start: number; end: number };
	encoding?: "utf-8" | "binary";
	maxLines?: number;
	maxBytes?: number;
	signal?: AbortSignal;
}

export interface ReadLinesResult {
	lines: string[];
	startLine: number;
	etag: string | null;
	eol: "LF" | "CRLF" | "CR";
	bom: boolean;
	truncated: boolean;
	totalLines?: number;
}

export interface ReadBlobOptions {
	range?: { start: number; end: number };
	sizeOnly?: boolean;
	signal?: AbortSignal;
}

export interface ReadBlobResult {
	bytes: Uint8Array;
	size: number;
	etag: string | null;
	contentType?: string;
}

export interface ReadAstOptions {
	range?: { start: number; end: number };
	language?: string;
	minBodyLines?: number;
	minCommentLines?: number;
	signal?: AbortSignal;
}

export type AstSummary = ReadAstResponse;

export type { ImageMetadata, MarkitConversionResult };

export interface ListWorkspaceOptions {
	path: string;
	maxDepth: number;
	hidden?: boolean;
	gitignore?: boolean;
	collectAgentsMd?: boolean;
	timeoutMs?: number;
	signal?: AbortSignal;
}

export interface ListWorkspaceResult {
	entries: GlobMatch[];
	agentsMdFiles: string[];
	truncated: boolean;
}

export interface WriteOptions {
	ifMatch?: string | "*";
	etag?: string | null;
	signal?: AbortSignal;
}

export interface WriteResult {
	etag: string;
	written: number;
}
export interface StatResult {
	exists: boolean;
	kind: "file" | "dir" | "symlink" | "other";
	size: number;
	mtimeMs: number;
	linkKind?: "symlink";
	etag: string | null;
}

export interface ArchiveEntriesOptions {
	prefix?: string;
	limit?: number;
	signal?: AbortSignal;
}

export interface ArchiveEntry {
	path: string;
	kind: "file" | "dir";
	size: number;
	mtimeMs: number | null;
	compressedSize: number | null;
}

export interface ArchiveEntriesResult {
	entries: ArchiveEntry[];
	format: "zip" | "tar" | "tar.gz";
	truncated: boolean;
}
export interface ArchiveSnapshot extends AsyncDisposable {
	readonly format: "zip" | "tar" | "tar.gz";
	entries(opts?: { signal?: AbortSignal }): Promise<ArchiveEntry[]>;
	readEntry(name: string, opts?: { signal?: AbortSignal }): Promise<Uint8Array>;
	close(): Promise<void>;
}

export interface GlobRequest {
	patterns: string[];
	paths?: string[];
	includeHidden?: boolean;
	maxDepth?: number;
	types?: Array<"file" | "dir" | "symlink">;
	limit?: number;
	gitignore?: boolean;
	signal?: AbortSignal;
}

export interface GlobResult {
	entries: Array<{ path: string; type: "file" | "dir" | "symlink" | "other"; size: number; modified: number }>;
	truncated: boolean;
}

export interface GrepRequest {
	pattern: string;
	paths: string[];
	ignoreCase?: boolean;
	multiline?: boolean;
	gitignore?: boolean;
	contextLines?: number;
	contextBefore?: number;
	contextAfter?: number;
	maxMatches?: number;
	signal?: AbortSignal;
}

export interface GrepSummary {
	type: "summary";
	limitReached: boolean;
	truncated?: boolean;
}

export type GrepHit = GrepRecord;

export interface GrepAstRequest {
	pattern: string;
	paths: string[];
	language?: string;
	strictness?: "smart" | "relaxed" | "strict";
	limit?: number;
	signal?: AbortSignal;
}

export interface GrepAstHit {
	file: string;
	range: { start: number; end: number };
	column: number;
	endColumn: number;
	matched: string;
	meta: Record<string, string>;
}
export interface GrepAstParseError {
	file?: string;
	message: string;
}

export interface GrepAstSummary {
	type: "summary";
	parseErrors?: GrepAstParseError[];
	filesSearched?: number;
	limitReached?: boolean;
}

export type KernelHandleConfig = EvalHandleConfig;

export interface KernelConfig extends KernelHandleConfig {
	transport?: "stdio" | "jupyter";
	idleTimeoutMs?: number;
	signal?: AbortSignal;
}

export interface KernelExecRequest extends EvalExecRequest {
	cwd?: string;
	signal?: AbortSignal;
}
export type KernelEvent = EvalEvent;

export interface KernelStatus {
	name: string;
	state: "starting" | "ready" | "error" | "closed";
	config: KernelConfig;
}

export interface LspConfig extends RwpLspHandleConfig {
	signal?: AbortSignal;
}

export interface LspStatus {
	name: string;
	state: "starting" | "ready" | "error" | "closed";
	config?: LspConfig;
	capabilities?: LspServerCapabilities;
	projectLoaded?: boolean;
	openFiles?: string[];
	diagnostics?: Map<string, Diagnostic[]>;
}

export interface DapConfig extends RwpDapHandleConfig {
	signal?: AbortSignal;
}

export interface DapStatus {
	name: string;
	state: "starting" | "ready" | "error" | "closed";
	config?: DapConfig;
}

export type BrowserConfig = RwpCdpHandleConfig & {
	signal?: AbortSignal;
};

export interface BrowserStatus {
	name: string;
	state: "starting" | "ready" | "error" | "closed";
	cdpUrl: string;
}

export interface EditAstRequest extends Omit<RwpEditAstRequest, "ops"> {
	ops?: AstOp[];
	rules?: AstOp[];
	language?: string;
	dryRun?: boolean;
	signal?: AbortSignal;
}
export type EditResult = RwpEditResult;

export interface AstEditHunk extends Omit<RwpAstEditHunk, "before_start" | "before_lines" | "after_lines"> {
	beforeStart: number;
	beforeLines: string[];
	afterLines: string[];
}

export interface AstEditFileChange extends Omit<RwpAstEditFileChange, "hunks" | "before_lines" | "after_lines"> {
	beforeLines: string[];
	afterLines: string[];
	hunks: AstEditHunk[];
}

export interface AstEditResult {
	changes: RwpAstFileChange[];
	fileChanges?: AstEditFileChange[];
	parseErrors: GrepAstParseError[];
	filesSearched: number;
	limitReached: boolean;
	written: boolean;
	truncated: boolean;
	exceededLimit: boolean;
}
// Wire mapping note: BashExecRequest uses camelCase in TS (`sessionKey`, `outputStreams`)
// and snake_case on the wire (`session_key`, `output_streams`). Exit minimizer fields
// likewise map camelCase ↔ snake_case (`originalLines` ↔ `original_lines`).
export interface BashExecRequest extends RwpBashExecRequest {
	outputStreams?: "merged" | "split";
	minimizer?: {
		enabled: boolean;
		aggressive?: boolean;
		minLines?: number;
		contextLines?: number;
	};
	sessionKey?: string;
	signal?: AbortSignal;
}
export interface WriteDbRequest extends RwpWriteDbRequest {
	signal?: AbortSignal;
}
export type WriteDbResponse = RwpWriteDbResponse;
export interface ReadDbRequest extends ReadDbQuery {
	signal?: AbortSignal;
}

// Backend edit calls use camelCase for client-only options; remote transport maps them to
// backend read/write preconditions instead of branching on local filesystem access.
export interface EditPatchRequest extends RwpEditPatchRequest {
	ifMatch?: string;
}

export interface EditReplaceRequest extends RwpEditReplaceRequest {
	ifMatch?: string;
	regex?: boolean;
	regexFlags?: string;
	all?: boolean;
}

// Divergence from rwp-client: the server has a table-listing variant for read.db.
export type ReadDbResponse =
	| RwpReadDbResponse
	| {
			tables: SqliteTableInfo[];
	  };

// Divergence from rwp-client: backend normalizes streamed shell events to the local tool shape.
export type BashEvent =
	| {
			type: "output";
			text: string;
	  }
	| {
			type: "stdout";
			data: string;
	  }
	| {
			type: "stderr";
			data: string;
	  }
	| {
			type: "exit";
			exitCode: number | null;
			signaled: boolean;
			cancelled?: boolean;
			timedOut?: boolean;
			minimizer?: {
				minimized: boolean;
				originalLines: number;
				minimizedLines: number;
				omittedLines: number;
				truncated: boolean;
				rawArtifact?: { kind: "path"; path: string } | { kind: "bytes"; bytes: Uint8Array };
			};
	  };

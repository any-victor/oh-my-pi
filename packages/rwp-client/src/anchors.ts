import * as hashline from "@oh-my-pi/pi-coding-agent/hashline";

export { computeLineHash, formatHashLines } from "@oh-my-pi/pi-coding-agent/hashline";

const INLINE_SELECTOR_RE = /:(\d+)(?:-(\d+)|\+(\d+))?$/;
const MAX_CACHED_ETAGS = 32;
const anchorCache = new Map<string, Map<number, string>>();

export interface AnchorSource {
	path: string;
	text: string;
	etag?: string | null;
}

export function anchoredText(source: AnchorSource): string {
	const startLine = inferStartLine(source.path);
	const lines = source.text.split("\n");

	if (source.etag == null) {
		return decorateUncached(lines, startLine);
	}

	const etagCache = getOrCreateEtagCache(source.etag);
	const decorated = new Array<string>(lines.length);
	for (let i = 0; i < lines.length; i += 1) {
		const lineNumber = startLine + i;
		let cached = etagCache.get(lineNumber);
		if (cached === undefined) {
			cached = formatDecoratedLine(lineNumber, lines[i] ?? "");
			etagCache.set(lineNumber, cached);
		}
		decorated[i] = cached;
	}
	return decorated.join("\n");
}

export function inferStartLine(path: string): number {
	const match = path.match(INLINE_SELECTOR_RE);
	if (!match) {
		return 1;
	}
	const start = Number.parseInt(match[1], 10);
	return Number.isFinite(start) && start > 0 ? start : 1;
}

export function __resetAnchorCache(): void {
	anchorCache.clear();
}

function decorateUncached(lines: string[], startLine: number): string {
	const decorated = new Array<string>(lines.length);
	for (let i = 0; i < lines.length; i += 1) {
		decorated[i] = formatDecoratedLine(startLine + i, lines[i] ?? "");
	}
	return decorated.join("\n");
}

function formatDecoratedLine(lineNumber: number, line: string): string {
	return `${lineNumber}${hashline.computeLineHash(lineNumber, line)}|${line}`;
}

function getOrCreateEtagCache(etag: string): Map<number, string> {
	const cached = anchorCache.get(etag);
	if (cached !== undefined) {
		anchorCache.delete(etag);
		anchorCache.set(etag, cached);
		return cached;
	}

	const created = new Map<number, string>();
	anchorCache.set(etag, created);
	if (anchorCache.size > MAX_CACHED_ETAGS) {
		const oldest = anchorCache.keys().next().value;
		if (oldest !== undefined) {
			anchorCache.delete(oldest);
		}
	}
	return created;
}

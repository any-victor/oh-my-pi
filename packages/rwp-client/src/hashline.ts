import bigrams from "./bigrams.json" with { type: "json" };

const SIGNIFICANT_CONTENT_RE = /[\p{L}\p{N}]/u;

/** Compute the legacy two-character hash used by RWP line anchors. */
export function computeLineHash(lineNumber: number, line: string): string {
	const normalized = line.replace(/\r/g, "").trimEnd();
	const seed = SIGNIFICANT_CONTENT_RE.test(normalized) ? 0 : lineNumber;
	return bigrams[Bun.hash.xxHash32(normalized, seed) % bigrams.length] ?? "aa";
}

/** Format text as legacy `LINE+HASH|TEXT` rows. */
export function formatHashLines(text: string, startLine = 1): string {
	return text
		.split("\n")
		.map((line, index) => {
			const lineNumber = startLine + index;
			return `${lineNumber}${computeLineHash(lineNumber, line)}|${line}`;
		})
		.join("\n");
}

import { afterEach, describe, expect, it, vi } from "bun:test";
import fs from "node:fs/promises";
import { createRequire } from "node:module";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { convertBufferWithMarkit } from "@oh-my-pi/pi-coding-agent/utils/markit";
import { logger } from "@oh-my-pi/pi-utils";

function warningPdf(): Uint8Array {
	const objects: string[] = [];
	function add(body: string): void {
		objects.push(body);
	}

	const pageText = "/P <</MCID 0>> BDC\nBT /F1 24 Tf 72 720 Td (Tagged PDF repro text) Tj ET\nEMC\n";
	add("<< /Type /Catalog /Pages 2 0 R /MarkInfo << /Marked true >> /StructTreeRoot 8 0 R >>");
	add("<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
	add(
		"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R /StructParents 0 /Annots [9 0 R] >>",
	);
	add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
	add(`<< /Length ${pageText.length} >>\nstream\n${pageText}endstream`);
	add("<< /Nums [0 [7 0 R]] >>");
	add("<< /Type /StructElem /S /P /P 8 0 R /Pg 3 0 R /K 99 >>");
	add("<< /Type /StructTreeRoot /K [7 0 R] /ParentTree 6 0 R /ParentTreeNextKey 1 >>");
	add("<< /Type /Annot /Subtype /Screen /Rect [72 650 200 700] /T (movie) >>");

	let pdf = "%PDF-1.7\n";
	const offsets = [0];
	for (let i = 0; i < objects.length; i++) {
		offsets.push(Buffer.byteLength(pdf));
		pdf += `${i + 1} 0 obj\n${objects[i]}\nendobj\n`;
	}

	const xref = Buffer.byteLength(pdf);
	pdf += `xref\n0 ${objects.length + 1}\n0000000000 65535 f \n`;
	for (let i = 1; i < offsets.length; i++) {
		pdf += `${String(offsets[i]).padStart(10, "0")} 00000 n \n`;
	}
	pdf += `trailer\n<< /Size ${objects.length + 1} /Root 1 0 R >>\nstartxref\n${xref}\n%%EOF\n`;

	return new TextEncoder().encode(pdf);
}

async function convertWithCompiledMarkit(): Promise<string> {
	const root = await fs.mkdtemp(path.join(os.tmpdir(), "omp-mupdf-compiled-"));
	const entry = path.join(root, "convert.ts");
	const output = path.join(root, "convert");
	const markitPath = fileURLToPath(new URL("../../src/utils/markit.ts", import.meta.url));
	const mupdfWasmPath = path.join(path.dirname(createRequire(import.meta.url).resolve("mupdf")), "mupdf-wasm.wasm");
	const nativeAddonPath = fileURLToPath(
		new URL(`../../../natives/native/pi_natives.${process.platform}-${process.arch}.node`, import.meta.url),
	);
	try {
		await fs.writeFile(
			entry,
			`import { readFileSync } from "node:fs";
import wasmPath from ${JSON.stringify(mupdfWasmPath)} with { type: "file" };
globalThis.$libmupdf_wasm_Module = { wasmBinary: readFileSync(wasmPath) };
// The compiled entry must dynamically load markit so Bun lowers the lazy converter boundary.
const { convertBufferWithMarkit } = await import(${JSON.stringify(markitPath)});
const results = await Promise.all(
	Array.from({ length: 16 }, () =>
		convertBufferWithMarkit(new Uint8Array(${JSON.stringify([...warningPdf()])}), ".pdf", undefined, { useCache: false }),
	),
);
for (const result of results) {
	if (!result.ok) throw new Error(result.error);
	if (!result.content.includes("Tagged PDF repro text")) throw new Error("missing converted text");
}
process.stdout.write("compiled conversion succeeded");
`,
		);
		const build = await Bun.build({
			entrypoints: [entry],
			root: fileURLToPath(new URL("../../../..", import.meta.url)),
			external: ["fastembed", "onnxruntime-node"],
			compile: { outfile: output },
			throw: false,
		});
		expect(build.success).toBe(true);
		await fs.copyFile(nativeAddonPath, path.join(root, path.basename(nativeAddonPath)));

		const process = Bun.spawn([output], { stdout: "pipe", stderr: "pipe" });
		const [exitCode, stdout, stderr] = await Promise.all([
			process.exited,
			new Response(process.stdout).text(),
			new Response(process.stderr).text(),
		]);
		if (exitCode !== 0) throw new Error(`compiled converter failed:\n${stderr}`);
		expect(stderr).toBe("");
		return stdout;
	} finally {
		await fs.rm(root, { force: true, recursive: true });
	}
}

describe("markit MuPDF warnings", () => {
	afterEach(() => {
		vi.restoreAllMocks();
	});

	it("initializes MuPDF once in a compiled binary under concurrent PDF conversions", async () => {
		expect(await convertWithCompiledMarkit()).toBe("compiled conversion succeeded");
	});

	it("routes recoverable PDF warnings to the file logger", async () => {
		const consoleError = vi.spyOn(console, "error").mockImplementation(() => undefined);
		const debug = vi.spyOn(logger, "debug").mockImplementation(() => undefined);

		const result = await convertBufferWithMarkit(warningPdf(), ".pdf", undefined, { useCache: false });

		expect(result.ok).toBe(true);
		expect(result.content).toContain("Tagged PDF repro text");
		expect(consoleError).not.toHaveBeenCalled();
		expect(
			debug.mock.calls.some(([message, metadata]) => {
				if (message !== "mupdf wasm output" || typeof metadata !== "object" || metadata === null) return false;
				if (!("stream" in metadata) || metadata.stream !== "stderr") return false;
				return "message" in metadata && String(metadata.message).includes("Screen annotations");
			}),
		).toBe(true);
	});
});

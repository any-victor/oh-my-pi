import { afterEach, describe, expect, test, vi } from "bun:test";
import * as hashline from "@oh-my-pi/pi-coding-agent/hashline";
import { __resetAnchorCache, anchoredText, computeLineHash } from "../src/anchors";

afterEach(() => {
	__resetAnchorCache();
	vi.restoreAllMocks();
});

describe("anchoredText", () => {
	test("reuses decorated lines for repeated reads with the same etag", () => {
		const source = { path: "src/foo.ts", text: "alpha\nbeta", etag: "etag-1" };
		const expected = `1${computeLineHash(1, "alpha")}|alpha\n2${computeLineHash(2, "beta")}|beta`;
		const spy = vi.spyOn(hashline, "computeLineHash");

		const first = anchoredText(source);
		expect(first).toBe(expected);
		expect(spy).toHaveBeenCalledTimes(2);

		spy.mockClear();
		const second = anchoredText(source);
		expect(second).toBe(expected);
		expect(spy).not.toHaveBeenCalled();
	});

	test("does not consult cache entries from a different etag", () => {
		const expected = `1${computeLineHash(1, "alpha")}|alpha`;
		anchoredText({ path: "src/foo.ts", text: "alpha", etag: "etag-a" });
		const spy = vi.spyOn(hashline, "computeLineHash");

		const decorated = anchoredText({ path: "src/foo.ts", text: "alpha", etag: "etag-b" });
		expect(decorated).toBe(expected);
		expect(spy).toHaveBeenCalledTimes(1);
	});

	test("evicts the least recently used etag when the cache exceeds 32 entries", () => {
		for (let i = 1; i <= 32; i += 1) {
			anchoredText({ path: "src/foo.ts", text: `line-${i}`, etag: `etag-${i}` });
		}

		const spy = vi.spyOn(hashline, "computeLineHash");

		anchoredText({ path: "src/foo.ts", text: "line-1", etag: "etag-1" });
		expect(spy).not.toHaveBeenCalled();

		anchoredText({ path: "src/foo.ts", text: "line-33", etag: "etag-33" });
		expect(spy).toHaveBeenCalledTimes(1);

		spy.mockClear();
		anchoredText({ path: "src/foo.ts", text: "line-2", etag: "etag-2" });
		expect(spy).toHaveBeenCalledTimes(1);

		spy.mockClear();
		anchoredText({ path: "src/foo.ts", text: "line-1", etag: "etag-1" });
		expect(spy).not.toHaveBeenCalled();
	});

	test("bypasses the cache when etag is null or undefined", () => {
		const spy = vi.spyOn(hashline, "computeLineHash");

		anchoredText({ path: "src/foo.ts", text: "alpha", etag: null });
		anchoredText({ path: "src/foo.ts", text: "alpha", etag: null });
		anchoredText({ path: "src/foo.ts", text: "alpha" });
		anchoredText({ path: "src/foo.ts", text: "alpha" });

		expect(spy).toHaveBeenCalledTimes(4);
	});
});

/* eslint-disable */
/* biome-ignore-all lint: generated file */
export interface paths {
	"/cdp/{name}": {
		parameters: {
			query?: never;
			header?: never;
			path?: never;
			cookie?: never;
		};
		get: operations["get_cdp"];
		put: operations["put_cdp"];
		post?: never;
		delete: operations["delete_cdp"];
		options?: never;
		head?: never;
		patch?: never;
		trace?: never;
	};
	"/dap/{name}": {
		parameters: {
			query?: never;
			header?: never;
			path?: never;
			cookie?: never;
		};
		get: operations["get_dap"];
		put: operations["put_dap"];
		post?: never;
		delete: operations["delete_dap"];
		options?: never;
		head?: never;
		patch?: never;
		trace?: never;
	};
	"/eval/{name}": {
		parameters: {
			query?: never;
			header?: never;
			path?: never;
			cookie?: never;
		};
		get: operations["get_eval"];
		put: operations["put_eval"];
		post: operations["exec_eval"];
		delete: operations["delete_eval"];
		options?: never;
		head?: never;
		patch?: never;
		trace?: never;
	};
	"/lsp/{name}": {
		parameters: {
			query?: never;
			header?: never;
			path?: never;
			cookie?: never;
		};
		get: operations["get_lsp"];
		put: operations["put_lsp"];
		post?: never;
		delete: operations["delete_lsp"];
		options?: never;
		head?: never;
		patch?: never;
		trace?: never;
	};
	"/sessions": {
		parameters: {
			query?: never;
			header?: never;
			path?: never;
			cookie?: never;
		};
		get?: never;
		put?: never;
		post: operations["create_session"];
		delete?: never;
		options?: never;
		head?: never;
		patch?: never;
		trace?: never;
	};
	"/sessions/{id}": {
		parameters: {
			query?: never;
			header?: never;
			path?: never;
			cookie?: never;
		};
		get?: never;
		put?: never;
		post?: never;
		delete: operations["delete_session"];
		options?: never;
		head?: never;
		patch?: never;
		trace?: never;
	};
	"/sessions/{id}/bash.exec": {
		parameters: {
			query?: never;
			header?: never;
			path?: never;
			cookie?: never;
		};
		get?: never;
		put?: never;
		post: operations["bash_exec"];
		delete?: never;
		options?: never;
		head?: never;
		patch?: never;
		trace?: never;
	};
	"/sessions/{id}/cwd": {
		parameters: {
			query?: never;
			header?: never;
			path?: never;
			cookie?: never;
		};
		get?: never;
		put: operations["set_cwd"];
		post?: never;
		delete?: never;
		options?: never;
		head?: never;
		patch?: never;
		trace?: never;
	};
	"/sessions/{id}/edit.ast": {
		parameters: {
			query?: never;
			header?: never;
			path?: never;
			cookie?: never;
		};
		get?: never;
		put?: never;
		post: operations["edit_ast"];
		delete?: never;
		options?: never;
		head?: never;
		patch?: never;
		trace?: never;
	};
	"/sessions/{id}/edit.patch": {
		parameters: {
			query?: never;
			header?: never;
			path?: never;
			cookie?: never;
		};
		get?: never;
		put?: never;
		post: operations["edit_patch"];
		delete?: never;
		options?: never;
		head?: never;
		patch?: never;
		trace?: never;
	};
	"/sessions/{id}/edit.replace": {
		parameters: {
			query?: never;
			header?: never;
			path?: never;
			cookie?: never;
		};
		get?: never;
		put?: never;
		post: operations["edit_replace"];
		delete?: never;
		options?: never;
		head?: never;
		patch?: never;
		trace?: never;
	};
	"/sessions/{id}/env": {
		parameters: {
			query?: never;
			header?: never;
			path?: never;
			cookie?: never;
		};
		get?: never;
		put?: never;
		post?: never;
		delete?: never;
		options?: never;
		head?: never;
		patch: operations["patch_env"];
		trace?: never;
	};
	"/sessions/{id}/events": {
		parameters: {
			query?: never;
			header?: never;
			path?: never;
			cookie?: never;
		};
		get: operations["events"];
		put?: never;
		post?: never;
		delete?: never;
		options?: never;
		head?: never;
		patch?: never;
		trace?: never;
	};
	"/sessions/{id}/glob": {
		parameters: {
			query?: never;
			header?: never;
			path?: never;
			cookie?: never;
		};
		get: operations["glob"];
		put?: never;
		post?: never;
		delete?: never;
		options?: never;
		head?: never;
		patch?: never;
		trace?: never;
	};
	"/sessions/{id}/grep": {
		parameters: {
			query?: never;
			header?: never;
			path?: never;
			cookie?: never;
		};
		get: operations["grep"];
		put?: never;
		post?: never;
		delete?: never;
		options?: never;
		head?: never;
		patch?: never;
		trace?: never;
	};
	"/sessions/{id}/grep.ast": {
		parameters: {
			query?: never;
			header?: never;
			path?: never;
			cookie?: never;
		};
		get: operations["grep_ast"];
		put?: never;
		post?: never;
		delete?: never;
		options?: never;
		head?: never;
		patch?: never;
		trace?: never;
	};
	"/sessions/{id}/read.ast": {
		parameters: {
			query?: never;
			header?: never;
			path?: never;
			cookie?: never;
		};
		get: operations["read_ast"];
		put?: never;
		post?: never;
		delete?: never;
		options?: never;
		head?: never;
		patch?: never;
		trace?: never;
	};
	"/sessions/{id}/read.blob": {
		parameters: {
			query?: never;
			header?: never;
			path?: never;
			cookie?: never;
		};
		get: operations["read_blob"];
		put?: never;
		post?: never;
		delete?: never;
		options?: never;
		head?: never;
		patch?: never;
		trace?: never;
	};
	"/sessions/{id}/read.db": {
		parameters: {
			query?: never;
			header?: never;
			path?: never;
			cookie?: never;
		};
		get: operations["read_db"];
		put?: never;
		post?: never;
		delete?: never;
		options?: never;
		head?: never;
		patch?: never;
		trace?: never;
	};
	"/sessions/{id}/read.lines": {
		parameters: {
			query?: never;
			header?: never;
			path?: never;
			cookie?: never;
		};
		get: operations["read_lines"];
		put?: never;
		post?: never;
		delete?: never;
		options?: never;
		head?: never;
		patch?: never;
		trace?: never;
	};
	"/sessions/{id}/write.blob": {
		parameters: {
			query?: never;
			header?: never;
			path?: never;
			cookie?: never;
		};
		get?: never;
		put: operations["write_blob"];
		post?: never;
		delete?: never;
		options?: never;
		head?: never;
		patch?: never;
		trace?: never;
	};
	"/sessions/{id}/write.db": {
		parameters: {
			query?: never;
			header?: never;
			path?: never;
			cookie?: never;
		};
		get?: never;
		put?: never;
		post: operations["write_db"];
		delete?: never;
		options?: never;
		head?: never;
		patch?: never;
		trace?: never;
	};
	"/sessions/{id}/write.lines": {
		parameters: {
			query?: never;
			header?: never;
			path?: never;
			cookie?: never;
		};
		get?: never;
		put: operations["write_lines"];
		post?: never;
		delete?: never;
		options?: never;
		head?: never;
		patch?: never;
		trace?: never;
	};
}
export type webhooks = Record<string, never>;
export interface components {
	schemas: {
		AstEditResult: {
			changes: components["schemas"]["AstFileChange"][];
			file_changes: components["schemas"]["AstEditFileChange"][];
			files_searched: number;
			limit_reached: boolean;
			parse_errors: {
				file?: string | null;
				message: string;
			}[];
			written: boolean;
			truncated: boolean;
			exceeded_limit: boolean;
		};
		AstFileChange: {
			path: string;
			/** Format: int32 */
			replacements: number;
			diff: string;
		};
		AstEditFileChange: {
			path: string;
			replacements: number;
			before_lines: string[];
			after_lines: string[];
			hunks: components["schemas"]["AstEditHunk"][];
		};
		AstEditHunk: {
			/** Format: int32 */
			before_start: number;
			before_lines: string[];
			after_lines: string[];
		};
		AstOp: {
			pat: string;
			out: string;
		};
		BashExecRequest: {
			command: string;
			cwd?: string | null;
			env?: {
				[key: string]: string;
			};
			pty?: boolean;
			/** Format: int64 */
			timeout_ms?: number | null;
		};
		CdpHandleResponse: {
			name: string;
			kind: components["schemas"]["CdpHandleResponseKind"];
			ws_url: string;
			/** Format: int32 */
			ref_count: number;
			/** Format: int64 */
			last_active_ms: number;
			args?: string[];
			headless?: boolean | null;
			user_prefs?: {
				[key: string]: unknown;
			} | null;
		};
		/** @enum {string} */
		CdpHandleResponseKind: "spawned" | "attached";
		CreateSessionRequest: {
			cwd?: string | null;
			env?: {
				[key: string]: string;
			};
		};
		CreateSessionResponse: {
			/** Format: uuid */
			id: string;
		};
		/** @enum {string} */
		DapTransport: "stdio" | "tcp";
		EditAstRequest: {
			ops: components["schemas"]["AstOp"][];
			paths: string[];
		};
		/** @enum {string} */
		EditOp: "create" | "update";
		EditPatchRequest: {
			path: string;
			hunks: components["schemas"]["Hunk"][];
			if_match?: string | null;
		};
		EditReplaceRequest: {
			path: string;
			old: string;
			new: string;
			fuzzy?: boolean;
			if_match?: string | null;
			regex?: boolean;
			regex_flags?: string | null;
			all?: boolean;
		};
		EditResult: {
			diff: string;
			/** Format: int32 */
			first_changed_line?: number | null;
			op: components["schemas"]["EditOp"];
		};
		/**
		 * @description JSON body for any error response. Stable across endpoints so clients can
		 *     match on `code` (machine-readable) and surface `message` to humans.
		 */
		ErrorBody: {
			/** @description Stable error code, kebab-case (e.g. `not-found`, `etag-mismatch`). */
			code: string;
			/** @description Human-readable explanation. */
			message: string;
			/** @description Optional opaque detail object (per-error context). */
			detail?: unknown;
		};
		EvalExecRequest: {
			code: string;
			/** Format: int64 */
			timeout_ms?: number | null;
			store_history?: boolean;
		};
		EvalStatusResponse: {
			name: string;
			lang: string;
			status: components["schemas"]["KernelState"];
			/** Format: int32 */
			ref_count: number;
			transport?: "stdio" | "jupyter" | null;
			/** Format: int64 */
			idle_timeout_ms?: number | null;
		};
		Hunk: {
			/**
			 * Format: int32
			 * @description 1-based starting line.
			 */
			start: number;
			/**
			 * Format: int32
			 * @description Number of lines to delete starting at `start`.
			 */
			deleted: number;
			/** @description Replacement lines (no trailing newlines). */
			inserted: string[];
		};
		/** @enum {string} */
		KernelState: "starting" | "busy" | "idle";
		LspGetResponse: {
			name: string;
			initialized: boolean;
			capabilities: Record<string, never>;
			project_loaded: boolean;
			open_files: string[];
			diagnostics: {
				[key: string]: unknown;
			};
			/** Format: int32 */
			ref_count: number;
			/** Format: int64 */
			last_active_ms: number;
		};
		NamedHandleConfig:
			| {
					lang: string;
					kernelspec?: string | null;
					/** @enum {string} */
					kind: "eval";
			  }
			| {
					command: string;
					args?: string[];
					env?: {
						[key: string]: string;
					};
					root_uri?: string | null;
					initialization_options?: unknown;
					/** Format: int64 */
					idle_timeout_ms?: number | null;
					/** @enum {string} */
					kind: "lsp";
			  }
			| {
					command: string;
					args?: string[];
					env?: {
						[key: string]: string;
					};
					transport?: components["schemas"]["DapTransport"];
					host?: string | null;
					/** Format: int32 */
					port?: number | null;
					/** @enum {string} */
					kind: "dap";
			  }
			| {
					path?: string | null;
					args?: string[];
					headless?: boolean;
					user_prefs?: {
						[key: string]: unknown;
					} | null;
					/** @enum {string} */
					kind: "cdp-spawn";
			  }
			| {
					cdp_url: string;
					/** @enum {string} */
					kind: "cdp-attach";
			  };
		PatchEnvRequest: {
			/** @description `null` value unsets the key. Otherwise sets it. */
			env: {
				[key: string]: string | null;
			};
		};
		ReadAstResponse: {
			language?: string | null;
			parsed: boolean;
			elided: boolean;
			/** Format: int32 */
			total_lines: number;
			segments: components["schemas"]["ReadAstSegment"][];
		};
		ReadAstSegment: {
			kind: string;
			/** Format: int32 */
			start_line: number;
			/** Format: int32 */
			end_line: number;
			text?: string | null;
		};
		ReadDbQuery: {
			path: string;
			table?: string | null;
			key?: string | null;
			q?: string | null;
			where?: string | null;
			order?: string | null;
			/** Format: int64 */
			limit?: number | null;
			/** Format: int64 */
			offset?: number | null;
		};
		/**
		 * @description One push event delivered on `GET /sessions/{id}/events`. Serialized as a
		 *     single NDJSON record per event.
		 */
		SessionEvent:
			| {
					path: string;
					etag: string;
					/** @enum {string} */
					type: "file-changed";
			  }
			| {
					path: string;
					diagnostics: unknown;
					/** @enum {string} */
					type: "diagnostics";
			  }
			| {
					/** @enum {string} */
					type: "heartbeat";
			  };
		SetCwdRequest: {
			cwd: string;
		};
		/** @enum {string} */
		WriteDbOp: "insert" | "update" | "delete" | "exec";
		WriteDbRequest: {
			path: string;
			op: components["schemas"]["WriteDbOp"];
			table?: string | null;
			key?: string | null;
			row?: {
				[key: string]: unknown;
			} | null;
			sql?: string | null;
		};
		WriteDbResponse: {
			/** Format: int64 */
			affected: number;
		};
	};
	responses: never;
	parameters: never;
	requestBodies: never;
	headers: never;
	pathItems: never;
}
export type $defs = Record<string, never>;
export interface operations {
	get_cdp: {
		parameters: {
			query?: never;
			header?: never;
			path: {
				name: string;
			};
			cookie?: never;
		};
		requestBody?: never;
		responses: {
			101: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
			200: {
				headers: {
					[name: string]: unknown;
				};
				content: {
					"application/json": components["schemas"]["CdpHandleResponse"];
				};
			};
			404: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
		};
	};
	put_cdp: {
		parameters: {
			query?: never;
			header?: never;
			path: {
				name: string;
			};
			cookie?: never;
		};
		requestBody: {
			content: {
				"application/json": components["schemas"]["NamedHandleConfig"];
			};
		};
		responses: {
			200: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
			201: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
			400: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
		};
	};
	delete_cdp: {
		parameters: {
			query?: never;
			header?: never;
			path: {
				name: string;
			};
			cookie?: never;
		};
		requestBody?: never;
		responses: {
			204: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
			404: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
		};
	};
	get_dap: {
		parameters: {
			query?: never;
			header?: never;
			path: {
				name: string;
			};
			cookie?: never;
		};
		requestBody?: never;
		responses: {
			/** @description WebSocket upgrade */
			101: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
			404: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
		};
	};
	put_dap: {
		parameters: {
			query?: never;
			header?: never;
			path: {
				name: string;
			};
			cookie?: never;
		};
		requestBody: {
			content: {
				"application/json": components["schemas"]["NamedHandleConfig"];
			};
		};
		responses: {
			200: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
			201: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
			400: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
		};
	};
	delete_dap: {
		parameters: {
			query?: never;
			header?: never;
			path: {
				name: string;
			};
			cookie?: never;
		};
		requestBody?: never;
		responses: {
			204: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
			404: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
		};
	};
	get_eval: {
		parameters: {
			query?: never;
			header?: never;
			path: {
				name: string;
			};
			cookie?: never;
		};
		requestBody?: never;
		responses: {
			200: {
				headers: {
					[name: string]: unknown;
				};
				content: {
					"application/json": components["schemas"]["EvalStatusResponse"];
				};
			};
			404: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
		};
	};
	put_eval: {
		parameters: {
			query?: never;
			header?: never;
			path: {
				name: string;
			};
			cookie?: never;
		};
		requestBody: {
			content: {
				"application/json": components["schemas"]["NamedHandleConfig"];
			};
		};
		responses: {
			200: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
			201: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
			400: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
			409: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
		};
	};
	exec_eval: {
		parameters: {
			query?: never;
			header?: never;
			path: {
				name: string;
			};
			cookie?: never;
		};
		requestBody: {
			content: {
				"application/json": components["schemas"]["EvalExecRequest"];
			};
		};
		responses: {
			200: {
				headers: {
					[name: string]: unknown;
				};
				content: {
					"application/x-ndjson": unknown;
				};
			};
			404: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
		};
	};
	delete_eval: {
		parameters: {
			query?: never;
			header?: never;
			path: {
				name: string;
			};
			cookie?: never;
		};
		requestBody?: never;
		responses: {
			204: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
			404: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
		};
	};
	get_lsp: {
		parameters: {
			query?: never;
			header?: never;
			path: {
				name: string;
			};
			cookie?: never;
		};
		requestBody?: never;
		responses: {
			/** @description WebSocket upgrade */
			101: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
			200: {
				headers: {
					[name: string]: unknown;
				};
				content: {
					"application/json": components["schemas"]["LspGetResponse"];
				};
			};
			404: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
		};
	};
	put_lsp: {
		parameters: {
			query?: never;
			header?: never;
			path: {
				name: string;
			};
			cookie?: never;
		};
		requestBody: {
			content: {
				"application/json": components["schemas"]["NamedHandleConfig"];
			};
		};
		responses: {
			200: {
				headers: {
					[name: string]: unknown;
				};
				content: {
					"application/json": components["schemas"]["LspGetResponse"];
				};
			};
			201: {
				headers: {
					[name: string]: unknown;
				};
				content: {
					"application/json": components["schemas"]["LspGetResponse"];
				};
			};
			409: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
		};
	};
	delete_lsp: {
		parameters: {
			query?: never;
			header?: never;
			path: {
				name: string;
			};
			cookie?: never;
		};
		requestBody?: never;
		responses: {
			204: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
			404: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
		};
	};
	create_session: {
		parameters: {
			query?: never;
			header?: never;
			path?: never;
			cookie?: never;
		};
		requestBody: {
			content: {
				"application/json": components["schemas"]["CreateSessionRequest"];
			};
		};
		responses: {
			/** @description Session created */
			201: {
				headers: {
					[name: string]: unknown;
				};
				content: {
					"application/json": components["schemas"]["CreateSessionResponse"];
				};
			};
			/** @description Bad request */
			400: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
		};
	};
	delete_session: {
		parameters: {
			query?: never;
			header?: never;
			path: {
				id: string;
			};
			cookie?: never;
		};
		requestBody?: never;
		responses: {
			/** @description Deleted */
			204: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
			404: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
		};
	};
	bash_exec: {
		parameters: {
			query?: never;
			header?: never;
			path: {
				id: string;
			};
			cookie?: never;
		};
		requestBody: {
			content: {
				"application/json": components["schemas"]["BashExecRequest"];
			};
		};
		responses: {
			200: {
				headers: {
					[name: string]: unknown;
				};
				content: {
					"application/x-ndjson": unknown;
				};
			};
			404: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
		};
	};
	set_cwd: {
		parameters: {
			query?: never;
			header?: never;
			path: {
				id: string;
			};
			cookie?: never;
		};
		requestBody: {
			content: {
				"application/json": components["schemas"]["SetCwdRequest"];
			};
		};
		responses: {
			204: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
			400: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
			404: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
		};
	};
	edit_ast: {
		parameters: {
			query?: never;
			header?: never;
			path: {
				id: string;
			};
			cookie?: never;
		};
		requestBody: {
			content: {
				"application/json": components["schemas"]["EditAstRequest"];
			};
		};
		responses: {
			200: {
				headers: {
					[name: string]: unknown;
				};
				content: {
					"application/json": components["schemas"]["AstEditResult"];
				};
			};
			400: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
			404: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
		};
	};
	edit_patch: {
		parameters: {
			query?: never;
			header?: never;
			path: {
				id: string;
			};
			cookie?: never;
		};
		requestBody: {
			content: {
				"application/json": components["schemas"]["EditPatchRequest"];
			};
		};
		responses: {
			200: {
				headers: {
					[name: string]: unknown;
				};
				content: {
					"application/json": components["schemas"]["EditResult"];
				};
			};
			404: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
			412: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
		};
	};
	edit_replace: {
		parameters: {
			query?: never;
			header?: never;
			path: {
				id: string;
			};
			cookie?: never;
		};
		requestBody: {
			content: {
				"application/json": components["schemas"]["EditReplaceRequest"];
			};
		};
		responses: {
			200: {
				headers: {
					[name: string]: unknown;
				};
				content: {
					"application/json": components["schemas"]["EditResult"];
				};
			};
			404: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
			409: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
			412: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
		};
	};
	patch_env: {
		parameters: {
			query?: never;
			header?: never;
			path: {
				id: string;
			};
			cookie?: never;
		};
		requestBody: {
			content: {
				"application/json": components["schemas"]["PatchEnvRequest"];
			};
		};
		responses: {
			204: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
			404: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
		};
	};
	events: {
		parameters: {
			query?: never;
			header?: never;
			path: {
				id: string;
			};
			cookie?: never;
		};
		requestBody?: never;
		responses: {
			200: {
				headers: {
					[name: string]: unknown;
				};
				content: {
					"application/x-ndjson": unknown;
				};
			};
			404: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
		};
	};
	glob: {
		parameters: {
			query?: never;
			header?: never;
			path: {
				id: string;
			};
			cookie?: never;
		};
		requestBody?: never;
		responses: {
			200: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
			400: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
			404: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
		};
	};
	grep: {
		parameters: {
			query?: never;
			header?: never;
			path: {
				id: string;
			};
			cookie?: never;
		};
		requestBody?: never;
		responses: {
			200: {
				headers: {
					[name: string]: unknown;
				};
				content: {
					"application/x-ndjson": unknown;
				};
			};
			400: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
			404: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
		};
	};
	grep_ast: {
		parameters: {
			query?: never;
			header?: never;
			path: {
				id: string;
			};
			cookie?: never;
		};
		requestBody?: never;
		responses: {
			200: {
				headers: {
					[name: string]: unknown;
				};
				content: {
					"application/x-ndjson": unknown;
				};
			};
			400: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
			404: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
		};
	};
	read_ast: {
		parameters: {
			query?: never;
			header?: never;
			path: {
				id: string;
			};
			cookie?: never;
		};
		requestBody?: never;
		responses: {
			200: {
				headers: {
					[name: string]: unknown;
				};
				content: {
					"application/json": components["schemas"]["ReadAstResponse"];
				};
			};
			404: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
			415: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
		};
	};
	read_blob: {
		parameters: {
			query?: never;
			header?: never;
			path: {
				id: string;
			};
			cookie?: never;
		};
		requestBody?: never;
		responses: {
			200: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
			206: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
			400: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
			404: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
		};
	};
	read_db: {
		parameters: {
			query?: never;
			header?: never;
			path: {
				id: string;
			};
			cookie?: never;
		};
		requestBody?: never;
		responses: {
			200: {
				headers: {
					[name: string]: unknown;
				};
				content: {
					"application/json": unknown;
				};
			};
			400: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
			404: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
		};
	};
	read_lines: {
		parameters: {
			query?: never;
			header?: never;
			path: {
				id: string;
			};
			cookie?: never;
		};
		requestBody?: never;
		responses: {
			200: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
			400: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
			404: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
		};
	};
	write_blob: {
		parameters: {
			query: {
				/** @description Path relative to the session cwd */
				path: string;
			};
			header?: never;
			path: {
				id: string;
			};
			cookie?: never;
		};
		requestBody: {
			content: {
				"application/octet-stream": string;
			};
		};
		responses: {
			204: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
			404: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
			412: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
		};
	};
	write_db: {
		parameters: {
			query?: never;
			header?: never;
			path: {
				id: string;
			};
			cookie?: never;
		};
		requestBody: {
			content: {
				"application/json": components["schemas"]["WriteDbRequest"];
			};
		};
		responses: {
			200: {
				headers: {
					[name: string]: unknown;
				};
				content: {
					"application/json": components["schemas"]["WriteDbResponse"];
				};
			};
			400: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
			404: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
		};
	};
	write_lines: {
		parameters: {
			query: {
				/** @description Path relative to the session cwd */
				path: string;
			};
			header?: never;
			path: {
				id: string;
			};
			cookie?: never;
		};
		requestBody: {
			content: {
				"text/plain": string;
			};
		};
		responses: {
			204: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
			404: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
			/** @description ETag mismatch */
			412: {
				headers: {
					[name: string]: unknown;
				};
				content?: never;
			};
		};
	};
}

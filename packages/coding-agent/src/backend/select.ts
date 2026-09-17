import type { Backend } from "./backend";
import { LocalBackend } from "./local-backend";
import { RemoteBackend, type RemoteBackendOptions } from "./remote-backend";

export interface BackendSelectOptions {
	cwd: string;
	env?: Record<string, string | undefined>;
	remote?: Pick<RemoteBackendOptions, "baseUrl" | "token" | "cwd">;
}

export function pickBackend(opts: BackendSelectOptions): Backend {
	if (opts.remote) {
		return new RemoteBackend(opts.remote);
	}
	const e = opts.env ?? process.env;
	const remoteUrl = e.RWP_URL;
	if (remoteUrl) {
		return new RemoteBackend({ baseUrl: remoteUrl, token: e.RWP_TOKEN });
	}
	return new LocalBackend({ cwd: opts.cwd });
}

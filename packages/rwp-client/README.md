# @oh-my-pi/rwp-client

TypeScript client for the Remote Workspace Protocol server.

## Install

This package is a workspace member in this monorepo.

## Regenerate types

```sh
bun --cwd packages/rwp-client run generate
```

The package commits `src/generated.ts`; consumers do not need codegen at install time.

## Start the server

```sh
cargo run -p rwp-server -- --bind 127.0.0.1:8080
```

## Quick start

```ts
import { RwpClient } from "@oh-my-pi/rwp-client";

const rwp = new RwpClient({ baseUrl: "http://127.0.0.1:8080" });
const session = await rwp.createSession({ cwd: process.cwd() });

const lines = await session.readLines("package.json", { range: "1-5" });
console.log(lines.text);
console.log(lines.decorated());

const nextText = `${lines.text}\n`;
await session.writeLines("tmp/example.txt", nextText, { ifMatch: lines.etag });

for await (const record of await session.grep("rwp", { paths: ["README.md"], context: 1 })) {
	console.log(record);
}

for await (const event of await session.bashExec({ command: "printf 'hi\\n'; exit 0" })) {
	if (event.type === "output") process.stdout.write(event.data);
	if (event.type === "exit") console.log(event.code);
}

await session.delete();
```

## Reads and writes

```ts
const session = await rwp.createSession({ cwd: "/repo" });

const read = await session.readLines("src/index.ts", { range: "10-20" });
await session.writeLines("src/index.ts", read.text.replace("old", "new"), {
	ifMatch: read.etag,
});

const blob = await session.readBlob("assets/logo.png");
await session.writeBlob("assets/logo-copy.png", blob.bytes);
```

## Grep stream

```ts
for await (const match of await session.grep("TODO", { paths: ["src/**/*.ts"], i: true })) {
	console.log(`${match.path}:${match.line} ${match.text}`);
}
```

## Eval tunnel

```ts
await rwp.putEval("py-main", { kind: "eval", lang: "python" });
for await (const event of await rwp.execEval("py-main", { code: "x = 1\nprint(x)" })) {
	console.log(event);
}
await rwp.deleteEval("py-main");
```

## LSP tunnel

```ts
await rwp.putLsp("main", {
	kind: "lsp",
	command: "typescript-language-server",
	args: ["--stdio"],
	root_uri: `file://${process.cwd()}`,
});

const status = await rwp.getLsp("main");
console.log(status.capabilities);

const ws = rwp.openLspWebSocket("main", {
	onMessage(message) {
		console.log(message);
	},
});

ws.send({
	jsonrpc: "2.0",
	id: 1,
	method: "initialize",
	params: {
		rootUri: `file://${process.cwd()}`,
		capabilities: {},
	},
});
```

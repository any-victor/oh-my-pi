/**
 * Regression test for the abort-guard on `EventController.sendCompletionNotification`.
 *
 * Bug: a user Ctrl+C on the `ask` tool selector throws `ToolAbortError`,
 * the turn ends with `stopReason === "aborted"`, and `#handleAgentEnd`
 * fires `sendCompletionNotification()`. Without a guard this produced a
 * misleading "Task complete" desktop toast for a turn that never actually
 * completed. The fix mirrors the `stopReason !== "aborted"` pattern already
 * used by `#currentContextTokens`, `#handleMessageEnd`, and the
 * retry / TTSR / compaction skip paths in `agent-session.ts`.
 */
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "bun:test";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import type { AssistantMessage } from "@oh-my-pi/pi-ai";
import { resetSettingsForTest, Settings, settings } from "@oh-my-pi/pi-coding-agent/config/settings";
import { SETTINGS_SCHEMA } from "@oh-my-pi/pi-coding-agent/config/settings-schema";
import { EventController } from "@oh-my-pi/pi-coding-agent/modes/controllers/event-controller";
import { initTheme } from "@oh-my-pi/pi-coding-agent/modes/theme/theme";
import type { InteractiveModeContext } from "@oh-my-pi/pi-coding-agent/modes/types";
import type { AgentSessionEvent } from "@oh-my-pi/pi-coding-agent/session/agent-session";
import { TERMINAL } from "@oh-my-pi/pi-tui";

beforeAll(() => {
	initTheme();
});

beforeEach(async () => {
	resetSettingsForTest();
	const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "omp-abortguard-"));
	await Settings.init({ inMemory: true, cwd: tempDir });
});

afterEach(() => {
	vi.restoreAllMocks();
	resetSettingsForTest();
});

type StopReason = "stop" | "aborted" | "error";

function makeAssistantMessage(stopReason: StopReason): AssistantMessage {
	return {
		role: "assistant",
		content: [{ type: "text", text: "hello" }],
		stopReason,
		usage: { inputTokens: 0, outputTokens: 0 },
		timestamp: Date.now(),
	} as unknown as AssistantMessage;
}

function makeContext(lastMessage: AssistantMessage | undefined): InteractiveModeContext {
	const sessionMock = {
		getLastAssistantMessage: () => lastMessage,
	};
	return {
		sessionManager: {
			getSessionName: () => "test-session",
		},
		session: sessionMock,
		viewSession: sessionMock,
	} as unknown as InteractiveModeContext;
}

function makeAgentEndEvent(messages: AssistantMessage[]): Extract<AgentSessionEvent, { type: "agent_end" }> {
	return { type: "agent_end", messages } as Extract<AgentSessionEvent, { type: "agent_end" }>;
}

/** Full context needed to drive `#handleAgentEnd` -> `#finishAgentEnd` end to end. */
function makeTurnEndContext(options: { lastAssistantMessage?: AssistantMessage } = {}): InteractiveModeContext {
	const session = {
		isStreaming: false,
		isCompacting: false,
		messages: [] as AssistantMessage[],
		getLastAssistantMessage: () => options.lastAssistantMessage,
		getContextUsage: () => undefined,
	};
	return {
		isInitialized: true,
		loadingAnimation: undefined,
		streamingComponent: undefined,
		streamingMessage: undefined,
		pendingTools: new Map<string, unknown>(),
		flushPendingModelSwitch: async () => {},
		ui: { requestRender: () => {} },
		chatContainer: { removeChild: () => {} },
		statusContainer: { clear: () => {} },
		statusLine: { markActivityEnd: () => {} },
		editor: { getText: () => "" },
		sessionManager: { getSessionName: () => "test-session" },
		session,
		viewSession: session,
	} as unknown as InteractiveModeContext;
}

describe("EventController.sendCompletionNotification — abort guard", () => {
	it("skips notification when the last assistant message stopReason === 'aborted'", () => {
		const spy = vi.spyOn(TERMINAL, "sendNotification").mockImplementation(() => {});
		settings.override("completion.notify", "on");
		const controller = new EventController(makeContext(makeAssistantMessage("aborted")));
		controller.sendCompletionNotification();
		expect(spy).toHaveBeenCalledTimes(0);
	});

	it("skips notification when the last assistant message stopReason === 'error'", () => {
		const spy = vi.spyOn(TERMINAL, "sendNotification").mockImplementation(() => {});
		settings.override("completion.notify", "on");
		const controller = new EventController(makeContext(makeAssistantMessage("error")));
		controller.sendCompletionNotification();
		expect(spy).toHaveBeenCalledTimes(0);
	});

	it("fires notification when stopReason === 'stop' (normal completion)", () => {
		const spy = vi.spyOn(TERMINAL, "sendNotification").mockImplementation(() => {});
		settings.override("completion.notify", "on");
		const controller = new EventController(makeContext(makeAssistantMessage("stop")));
		controller.sendCompletionNotification();
		expect(spy).toHaveBeenCalledTimes(1);
		// Completion now sends a structured notification (title=session, body="Complete").
		expect(spy).toHaveBeenCalledWith(expect.objectContaining({ body: "Complete", type: "completion" }));
	});

	it("fires notification when getLastAssistantMessage is absent (e.g. brand-new session)", () => {
		// Defensive: optional-chain `?.()` returns undefined; treat as 'no abort flag', proceed.
		const spy = vi.spyOn(TERMINAL, "sendNotification").mockImplementation(() => {});
		settings.override("completion.notify", "on");
		const controller = new EventController(makeContext(undefined));
		controller.sendCompletionNotification();
		expect(spy).toHaveBeenCalledTimes(1);
	});

	it("honors the existing completion.notify=off gate", () => {
		const spy = vi.spyOn(TERMINAL, "sendNotification").mockImplementation(() => {});
		settings.override("completion.notify", "off");
		const controller = new EventController(makeContext(makeAssistantMessage("stop")));
		controller.sendCompletionNotification();
		expect(spy).toHaveBeenCalledTimes(0);
	});
});

describe("EventController.sendErrorNotification", () => {
	it("defaults error notifications to opt-in", () => {
		expect(SETTINGS_SCHEMA["error.notify"].default).toBe("off");
	});

	it("fires an error notification when stopReason === 'error'", () => {
		const spy = vi.spyOn(TERMINAL, "sendNotification").mockImplementation(() => {});
		settings.override("error.notify", "on");
		const controller = new EventController(makeContext(undefined));
		controller.sendErrorNotification(makeAgentEndEvent([makeAssistantMessage("error")]));
		expect(spy).toHaveBeenCalledTimes(1);
		expect(spy).toHaveBeenCalledWith(
			expect.objectContaining({ body: "Stopped with error", type: "error", title: "test-session" }),
		);
	});

	it("reads the terminal turn from agent_end.messages, not the mutable active context", () => {
		// A classifier-refusal failure is pruned from `viewSession`'s active
		// context before `agent_end` fires (`#removeAssistantMessageFromActiveContext`
		// in agent-session.ts), so `viewSession.getLastAssistantMessage()` here
		// reflects a stale, non-error turn. The notification must still fire off
		// the event's own `messages`, not that stale snapshot.
		const spy = vi.spyOn(TERMINAL, "sendNotification").mockImplementation(() => {});
		settings.override("error.notify", "on");
		const controller = new EventController(makeContext(makeAssistantMessage("stop")));
		controller.sendErrorNotification(makeAgentEndEvent([makeAssistantMessage("error")]));
		expect(spy).toHaveBeenCalledTimes(1);
		expect(spy).toHaveBeenCalledWith(expect.objectContaining({ body: "Stopped with error", type: "error" }));
	});

	it("uses the last assistant message when agent_end carries multiple messages", () => {
		const spy = vi.spyOn(TERMINAL, "sendNotification").mockImplementation(() => {});
		settings.override("error.notify", "on");
		const controller = new EventController(makeContext(undefined));
		controller.sendErrorNotification(
			makeAgentEndEvent([makeAssistantMessage("stop"), makeAssistantMessage("error")]),
		);
		expect(spy).toHaveBeenCalledTimes(1);
	});

	it("honors error.notify=off without changing completion notifications", () => {
		const spy = vi.spyOn(TERMINAL, "sendNotification").mockImplementation(() => {});
		settings.override("error.notify", "off");
		settings.override("completion.notify", "on");

		const errorController = new EventController(makeContext(undefined));
		errorController.sendErrorNotification(makeAgentEndEvent([makeAssistantMessage("error")]));
		expect(spy).toHaveBeenCalledTimes(0);

		const completionController = new EventController(makeContext(makeAssistantMessage("stop")));
		completionController.sendCompletionNotification();
		expect(spy).toHaveBeenCalledTimes(1);
		expect(spy).toHaveBeenCalledWith(expect.objectContaining({ body: "Complete", type: "completion" }));
	});

	it("skips user-aborted turns", () => {
		const spy = vi.spyOn(TERMINAL, "sendNotification").mockImplementation(() => {});
		settings.override("error.notify", "on");
		const controller = new EventController(makeContext(undefined));
		controller.sendErrorNotification(makeAgentEndEvent([makeAssistantMessage("aborted")]));
		expect(spy).toHaveBeenCalledTimes(0);
	});

	it("skips normal completion turns", () => {
		const spy = vi.spyOn(TERMINAL, "sendNotification").mockImplementation(() => {});
		settings.override("error.notify", "on");
		const controller = new EventController(makeContext(undefined));
		controller.sendErrorNotification(makeAgentEndEvent([makeAssistantMessage("stop")]));
		expect(spy).toHaveBeenCalledTimes(0);
	});
});

describe("EventController — error notification through the real turn-end path (#handleAgentEnd)", () => {
	beforeEach(() => {
		// Isolate the error-notification assertion from the pre-existing
		// completion-notification side effect that also fires from
		// `#finishAgentEnd` on every settled turn.
		settings.override("completion.notify", "off");
	});

	it("fires when the dispatched turn settles with stopReason === 'error', even with a stale active-context snapshot", async () => {
		const spy = vi.spyOn(TERMINAL, "sendNotification").mockImplementation(() => {});
		settings.override("error.notify", "on");
		// viewSession (active context) reports no assistant at all — the shape a
		// classifier-refusal prune leaves behind — while the terminal agent_end
		// event still carries the failed turn.
		const controller = new EventController(makeTurnEndContext({ lastAssistantMessage: undefined }));
		await controller.handleEvent(makeAgentEndEvent([makeAssistantMessage("error")]));
		expect(spy).toHaveBeenCalledTimes(1);
		expect(spy).toHaveBeenCalledWith(expect.objectContaining({ body: "Stopped with error", type: "error" }));
	});

	it("skips notification when the dispatched turn settles with stopReason === 'aborted'", async () => {
		const spy = vi.spyOn(TERMINAL, "sendNotification").mockImplementation(() => {});
		settings.override("error.notify", "on");
		const controller = new EventController(makeTurnEndContext());
		await controller.handleEvent(makeAgentEndEvent([makeAssistantMessage("aborted")]));
		expect(spy).not.toHaveBeenCalled();
	});
});

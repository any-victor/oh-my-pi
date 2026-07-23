import type {
	AgentTool,
	AgentToolContext,
	AgentToolResult,
	AgentToolUpdateCallback,
	ToolApprovalDecision,
} from "@oh-my-pi/pi-agent-core";
import type { ComputerAction, ComputerSafetyCheck, ComputerToolCallMetadata } from "@oh-my-pi/pi-ai";
import type {
	DesktopAction,
	DesktopCapabilities,
	DesktopCapture,
	DesktopDisplay,
	DesktopSessionOptions,
} from "@oh-my-pi/pi-natives";
import { prompt, sanitizeText } from "@oh-my-pi/pi-utils";
import { type } from "arktype";
import computerDescription from "../prompts/tools/computer.md" with { type: "text" };
import { truncateForPrompt } from "./approval";
import { type ComputerController, ComputerSupervisor, registerComputerController } from "./computer/supervisor";
import type { ToolSession } from "./index";
import { ToolError, throwIfAborted } from "./tool-errors";

const computerSchema = type({
	"actions?": type("unknown[]").describe("ordered computer actions; provider-native calls supply these automatically"),
});

export type ComputerParams = typeof computerSchema.infer;

export interface ComputerToolDetails {
	width: number;
	height: number;
	backend: DesktopCapture["backend"];
	displayServer?: string;
	capturePermission: string;
	inputPermission: string;
	displays: DesktopDisplay[];
	capabilities?: DesktopCapabilities;
	actions: ComputerAction["type"][];
}

export type ComputerControllerFactory = (options: DesktopSessionOptions) => ComputerController;

function isNumber(value: unknown): value is number {
	return typeof value === "number" && Number.isFinite(value);
}

function isPoint(value: unknown): value is { x: number; y: number } {
	return (
		!!value &&
		typeof value === "object" &&
		isNumber((value as { x?: unknown }).x) &&
		isNumber((value as { y?: unknown }).y)
	);
}

function isStringArray(value: unknown): value is string[] {
	return Array.isArray(value) && value.every(item => typeof item === "string");
}

function isComputerAction(value: unknown): value is ComputerAction {
	if (!value || typeof value !== "object" || typeof (value as { type?: unknown }).type !== "string") return false;
	const action = value as Record<string, unknown>;
	switch (action.type) {
		case "click":
			return (
				isNumber(action.x) &&
				isNumber(action.y) &&
				["left", "right", "wheel", "back", "forward"].includes(String(action.button))
			);
		case "double_click":
			return isNumber(action.x) && isNumber(action.y) && (action.keys === null || isStringArray(action.keys));
		case "drag":
			return Array.isArray(action.path) && action.path.length > 0 && action.path.every(isPoint);
		case "keypress":
			return isStringArray(action.keys) && action.keys.length > 0;
		case "move":
			return isNumber(action.x) && isNumber(action.y);
		case "screenshot":
		case "wait":
			return true;
		case "scroll":
			return isNumber(action.x) && isNumber(action.y) && isNumber(action.scroll_x) && isNumber(action.scroll_y);
		case "type":
			return typeof action.text === "string";
		default:
			return false;
	}
}

function parseActions(value: unknown): ComputerAction[] {
	if (!Array.isArray(value) || value.length === 0) throw new ToolError("Computer call requires at least one action");
	if (!value.every(isComputerAction)) throw new ToolError("Computer call contains an invalid action");
	return value;
}

function toDesktopAction(action: ComputerAction): DesktopAction {
	switch (action.type) {
		case "click":
			return {
				type: "click",
				x: action.x,
				y: action.y,
				button: action.button,
				...(action.keys ? { keys: action.keys } : {}),
			};
		case "double_click":
			return {
				type: "double_click",
				x: action.x,
				y: action.y,
				...(action.keys ? { keys: action.keys } : {}),
			};
		case "drag":
			return { type: "drag", path: action.path, ...(action.keys ? { keys: action.keys } : {}) };
		case "keypress":
			return { type: "keypress", keys: action.keys };
		case "move":
			return { type: "move", x: action.x, y: action.y, ...(action.keys ? { keys: action.keys } : {}) };
		case "screenshot":
			return { type: "screenshot" };
		case "scroll":
			return {
				type: "scroll",
				x: action.x,
				y: action.y,
				scroll_x: action.scroll_x,
				scroll_y: action.scroll_y,
				...(action.keys ? { keys: action.keys } : {}),
			};
		case "type":
			return { type: "type", text: action.text };
		case "wait":
			return { type: "wait" };
	}
}

function callMetadata(context: AgentToolContext | undefined): ComputerToolCallMetadata | undefined {
	const metadata = context?.toolCall?.providerMetadata;
	return metadata?.type === "computer" ? metadata : undefined;
}

export function computerApproval(args: unknown): ToolApprovalDecision {
	const actions =
		args && typeof args === "object" && "actions" in args ? (args as { actions?: unknown }).actions : undefined;
	if (!Array.isArray(actions)) return "exec";
	return actions.every(action => {
		if (!action || typeof action !== "object") return false;
		const actionType = (action as { type?: unknown }).type;
		return actionType === "screenshot" || actionType === "wait";
	})
		? "read"
		: "exec";
}

function modifierSummary(keys: unknown): string {
	return isStringArray(keys) && keys.length > 0 ? ` keys=${JSON.stringify(keys)}` : "";
}

function approvalActionSummary(actions: unknown): string[] {
	if (!Array.isArray(actions)) return ["Actions: unavailable"];
	const lines = actions.slice(0, 12).map((value, index) => {
		if (!value || typeof value !== "object") return `${index + 1}. invalid`;
		const action = value as Record<string, unknown>;
		const type = typeof action.type === "string" ? action.type : "invalid";
		let detail: string;
		switch (type) {
			case "click":
				detail = `click button=${String(action.button)} at (${String(action.x)}, ${String(action.y)})${modifierSummary(action.keys)}`;
				break;
			case "double_click":
				detail = `double_click at (${String(action.x)}, ${String(action.y)})${modifierSummary(action.keys)}`;
				break;
			case "drag":
				detail = `drag path=${Array.isArray(action.path) ? action.path.map(point => (isPoint(point) ? `(${point.x}, ${point.y})` : "invalid")).join(" -> ") : "invalid"}${modifierSummary(action.keys)}`;
				break;
			case "keypress":
				detail = `keypress keys=${JSON.stringify(action.keys)}`;
				break;
			case "move":
				detail = `move to (${String(action.x)}, ${String(action.y)})${modifierSummary(action.keys)}`;
				break;
			case "scroll":
				detail = `scroll at (${String(action.x)}, ${String(action.y)}) delta=(${String(action.scroll_x)}, ${String(action.scroll_y)})${modifierSummary(action.keys)}`;
				break;
			case "type":
				detail = `type text=${JSON.stringify(action.text)}`;
				break;
			case "screenshot":
			case "wait":
				detail = type;
				break;
			default:
				detail = type;
		}
		return truncateForPrompt(sanitizeText(`${index + 1}. ${detail}`).replace(/[\r\n\t]+/g, " "), 240);
	});
	if (actions.length > 12) lines.push(`+${actions.length - 12} more actions`);
	return truncateForPrompt(lines.join("\n"), 2_000).split("\n");
}

export class ComputerTool implements AgentTool<typeof computerSchema, ComputerToolDetails> {
	readonly name = "computer";
	readonly native = { type: "computer" } as const;
	readonly label = "Computer";
	readonly loadMode = "essential" as const;
	readonly concurrency = "exclusive" as const;
	readonly summary = "Capture and control the host desktop through native OS APIs";
	readonly parameters = computerSchema;
	readonly strict = true;
	readonly approval = computerApproval;
	readonly formatApprovalDetails = (args: unknown): string[] => {
		const actions = args && typeof args === "object" ? (args as { actions?: unknown }).actions : undefined;
		return approvalActionSummary(actions);
	};
	readonly #controller: ComputerController;
	readonly #unregisterOwner: () => void;
	#closed = false;
	#description?: string;

	constructor(
		readonly session: ToolSession,
		createController: ComputerControllerFactory = options => new ComputerSupervisor(options),
	) {
		this.#controller = createController({
			backend: session.settings.get("computer.backend"),
			display: session.settings.get("computer.display"),
			maxWidth: session.settings.get("computer.maxWidth"),
			maxHeight: session.settings.get("computer.maxHeight"),
		});
		this.#unregisterOwner = registerComputerController(
			session.getEvalKernelOwnerId?.() ?? undefined,
			this.#controller,
		);
	}
	get description(): string {
		this.#description ??= prompt.render(computerDescription);
		return this.#description;
	}

	async execute(
		_toolCallId: string,
		params: ComputerParams,
		signal?: AbortSignal,
		_onUpdate?: AgentToolUpdateCallback<ComputerToolDetails>,
		context?: AgentToolContext,
	): Promise<AgentToolResult<ComputerToolDetails>> {
		throwIfAborted(signal);
		if (this.#closed) throw new ToolError("Computer session is closed");
		const metadata = callMetadata(context);
		const actions = parseActions(metadata?.actions ?? params.actions);
		const pendingSafetyChecks: ComputerSafetyCheck[] = metadata?.pendingSafetyChecks ?? [];
		if (pendingSafetyChecks.length > 0 && context?.providerSafetyApproved !== true) {
			throw new ToolError("Provider safety checks require interactive approval before computer input");
		}
		const capture = await this.#controller.execute(actions.map(toDesktopAction), signal);
		throwIfAborted(signal);
		const data = Buffer.from(capture.data).toBase64();
		return {
			content: [{ type: "image", data, mimeType: "image/png", detail: "original" }],
			details: {
				width: capture.width,
				height: capture.height,
				backend: capture.backend,
				displayServer: capture.displayServer,
				capturePermission: capture.capturePermission,
				inputPermission: capture.inputPermission,
				displays: capture.displays,
				capabilities: this.#controller.capabilities,
				actions: actions.map(action => action.type),
			},
			providerMetadata: {
				type: "computer",
				screenshot: { type: "computer_screenshot", image_url: `data:image/png;base64,${data}` },
				acknowledgedSafetyChecks: pendingSafetyChecks,
			},
		};
	}

	async close(): Promise<void> {
		if (this.#closed) return;
		this.#closed = true;
		this.#unregisterOwner();
		await this.#controller.close();
	}
}

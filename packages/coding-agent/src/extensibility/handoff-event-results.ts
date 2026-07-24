import type {
	SessionBeforeHandoffEvent,
	SessionBeforeHandoffResult,
	SessionHandoffGeneratedEvent,
	SessionHandoffGeneratedResult,
} from "./shared-events";

export function mergeSessionBeforeHandoffResult(
	event: SessionBeforeHandoffEvent,
	previous: SessionBeforeHandoffResult | undefined,
	update: SessionBeforeHandoffResult,
): { event: SessionBeforeHandoffEvent; result: SessionBeforeHandoffResult } {
	const result = { ...previous };
	if (update.cancel !== undefined) result.cancel = update.cancel;
	if (update.customInstructions !== undefined) result.customInstructions = update.customInstructions;
	if (update.additionalContext !== undefined) result.additionalContext = update.additionalContext;
	return {
		result,
		event: {
			...event,
			customInstructions: result.customInstructions ?? event.customInstructions,
			additionalContext: result.additionalContext ?? event.additionalContext,
		},
	};
}

export function mergeSessionHandoffGeneratedResult(
	event: SessionHandoffGeneratedEvent,
	previous: SessionHandoffGeneratedResult | undefined,
	update: SessionHandoffGeneratedResult,
): { event: SessionHandoffGeneratedEvent; result: SessionHandoffGeneratedResult } {
	const result = { ...previous };
	if (update.cancel !== undefined) result.cancel = update.cancel;
	if (update.document !== undefined) result.document = update.document;
	return {
		result,
		event: {
			...event,
			document: result.document ?? event.document,
		},
	};
}

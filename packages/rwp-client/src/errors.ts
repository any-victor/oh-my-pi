import type { ErrorBody } from "./types";

export class RwpError extends Error {
	readonly code: string;
	readonly status: number;
	readonly detail?: ErrorBody["detail"];

	constructor(status: number, body: ErrorBody) {
		super(body.message);
		this.name = new.target.name;
		this.code = body.code;
		this.status = status;
		this.detail = body.detail;
		Object.setPrototypeOf(this, new.target.prototype);
	}
}

export class NotFoundError extends RwpError {}
export class EtagMismatchError extends RwpError {}
export class BadRequestError extends RwpError {}

export function toRwpError(status: number, body: ErrorBody): RwpError {
	switch (body.code) {
		case "not-found":
			return new NotFoundError(status, body);
		case "etag-mismatch":
			return new EtagMismatchError(status, body);
		case "bad-request":
			return new BadRequestError(status, body);
		default:
			return new RwpError(status, body);
	}
}

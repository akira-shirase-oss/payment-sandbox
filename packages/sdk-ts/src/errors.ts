// Every way a call can end without a usable answer. Each message says what went wrong and
// what to do; `code` is stable for programs. SIMULATED MONEY ONLY.

/** The base of every error this SDK throws. */
export class SandboxError extends Error {
	readonly code: string;

	constructor(code: string, message: string) {
		super(message);
		this.code = code;
		this.name = new.target.name;
	}
}

/** The configuration was refused before anything was sent (D-068). */
export class ConfigError extends SandboxError {}

/** The sandbox refused the request with its error envelope (a 4xx, or a 5xx not retried). */
export class RefusedError extends SandboxError {
	readonly status: number;
	readonly requestId: string | null;

	constructor(status: number, code: string, message: string, requestId: string | null) {
		super(code, message);
		this.status = status;
		this.requestId = requestId;
	}
}

/**
 * A money-moving request got no usable answer after its retries (no answer at all, a server
 * error, or a proxy answering for a restarting server): it may or may not have been
 * applied. `settled` is false, as for an uncertain operation. Retry with the same
 * `idempotencyKey` to find out; never with a new one.
 */
export class UnknownOutcomeError extends SandboxError {
	readonly idempotencyKey: string;
	readonly attempts: number;
	/** Not known yet, like `MoneyResult.settled: false`: never treat it as failed or paid. */
	readonly settled = false as const;

	constructor(idempotencyKey: string, attempts: number, cause: string) {
		super(
			'unknown_outcome',
			`No usable answer from the sandbox after ${attempts} attempts (${cause}); the request may or may not have been applied. Retry it with idempotencyKey ${idempotencyKey} to find out (the sandbox answers from its record or finishes it, never charging twice), never with a new key.`
		);
		this.idempotencyKey = idempotencyKey;
		this.attempts = attempts;
	}
}

/** A read (or a request that cannot have been applied) got no answer. */
export class UnreachableError extends SandboxError {
	constructor(apiUrl: string, cause: string) {
		super('unreachable', `The sandbox at ${apiUrl} did not answer (${cause}). Check that sandbox-server is running.`);
	}
}

/** Something answered without `Sandbox-Simulated: true`: not a Payment Sandbox. */
export class NotASandboxError extends SandboxError {
	constructor(apiUrl: string) {
		super('not_a_sandbox', `${apiUrl} answered without Sandbox-Simulated: true, so it is not a Payment Sandbox; check apiUrl.`);
	}
}

/** A webhook delivery failed verification (D-020). */
export class WebhookVerificationError extends SandboxError {}

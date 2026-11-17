// Payment Sandbox's TypeScript SDK. SIMULATED MONEY ONLY: no real cards, balances or
// banking connection; it refuses production-looking configuration (D-068).
export { Sandbox, SANDBOX_NOTICE, newIdempotencyKey, retryable, backoffMs } from './client.ts';
export type { Currency, MoneyOptions, MoneyResult, NetworkScenario, Operation, OperationState, Payment, Project } from './client.ts';
export { checkConfig, DEFAULT_API_URL, DEFAULT_RETRY, REAL_PROCESSOR_HOSTS, isLoopbackHost, isRealProcessor } from './config.ts';
export type { RetryPolicy, SandboxOptions } from './config.ts';
export {
	ConfigError,
	NotASandboxError,
	RefusedError,
	SandboxError,
	UnknownOutcomeError,
	UnreachableError,
	WebhookVerificationError
} from './errors.ts';
export { verifyWebhook, signWebhook, SIGNATURE_HEADER, EVENT_ID_HEADER, TOLERANCE_SECONDS } from './webhooks.ts';
export type { SandboxEvent } from './webhooks.ts';

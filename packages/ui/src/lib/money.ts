// Money as the sandbox stores it: integer minor units and the currency's exponent (D-010).
// Formatting is string work on the integer, never floating point, so 0.1 + 0.2 never shows.

/** `minor` units with `exponent` decimals: 125000, 2 → "1,250.00"; 700, 3 → "0.700". */
export function formatMinor(minor: number, exponent: number): string {
	if (!Number.isSafeInteger(minor) || !Number.isInteger(exponent) || exponent < 0 || exponent > 6) {
		return '—';
	}
	const digits = String(Math.abs(minor)).padStart(exponent + 1, '0');
	const whole = exponent === 0 ? digits : digits.slice(0, digits.length - exponent);
	const fraction = exponent === 0 ? '' : `.${digits.slice(digits.length - exponent)}`;
	const grouped = whole.replace(/\B(?=(\d{3})+(?!\d))/g, ',');
	return `${minor < 0 ? '−' : ''}${grouped}${fraction}`;
}

/** The exponents of the sandbox's currencies (D-010). */
export const EXPONENTS: Readonly<Record<string, number>> = { USD: 2, EUR: 2, JPY: 0, KWD: 3 };

/** `minor` in `currency`, with its code: "12.50 USD". Unknown currencies show the raw units. */
export function formatMoney(minor: number, currency: string): string {
	const exponent = EXPONENTS[currency];
	return exponent === undefined ? `${minor} ${currency} (minor units)` : `${formatMinor(minor, exponent)} ${currency}`;
}

import { formatMinor } from '@sandbox/ui/money';

const EXPONENT: Record<string, number> = { USD: 2, EUR: 2, JPY: 0, KWD: 3 };

/** Minor units of `currency`, exactly (integer string work, never floats). */
export const money = (minor: number, currency: string): string =>
	`${formatMinor(minor, EXPONENT[currency] ?? 0)} ${currency}`;

/** Unix seconds as a short local date and time. */
export const when = (unix: number): string =>
	new Date(unix * 1000).toLocaleString(undefined, { dateStyle: 'medium', timeStyle: 'medium' });

/** An id shortened for a table cell (the full id is in the link and its title). */
export const short = (id: string): string => id.slice(0, 8);

/** Account codes as words. */
export const account = (code: string): string => code.replaceAll('_', ' ');

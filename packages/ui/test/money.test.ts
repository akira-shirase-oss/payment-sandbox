// Amounts are integers with an exponent; formatting never goes through floating point.
import { expect, test } from 'vitest';
import { formatMinor, formatMoney } from '../src/lib/money.ts';

test('minor units are placed by the exponent, grouped, and exact', () => {
	expect(formatMinor(1_250, 2)).toBe('12.50');
	expect(formatMinor(5, 2)).toBe('0.05');
	expect(formatMinor(700, 3)).toBe('0.700');
	expect(formatMinor(900, 0)).toBe('900');
	expect(formatMinor(123_456_789, 2)).toBe('1,234,567.89');
	expect(formatMinor(1_000_000_000_000, 2)).toBe('10,000,000,000.00');
	expect(formatMinor(-350, 2)).toBe('−3.50');
	expect(formatMinor(0, 3)).toBe('0.000');
	// 0.1 + 0.2 in minor units is 30 minor units, and shows as such.
	expect(formatMinor(10 + 20, 2)).toBe('0.30');
});

test('anything that is not a safe integer is not shown as money', () => {
	expect(formatMinor(1.5, 2)).toBe('—');
	expect(formatMinor(Number.MAX_SAFE_INTEGER + 2, 2)).toBe('—');
	expect(formatMinor(Number.NaN, 2)).toBe('—');
	expect(formatMoney(1_250, 'USD')).toBe('12.50 USD');
	expect(formatMoney(1_250, 'JPY')).toBe('1,250 JPY');
	expect(formatMoney(1_250, 'XYZ')).toBe('1250 XYZ (minor units)');
});

/** Jurisdiction-specific tax policy used by invoice total calculation. */
export function taxRateFor(jurisdiction: string): number {
  if (jurisdiction === 'IL') return 0.0625;
  if (jurisdiction === 'CA') return 0.0725;
  return 0;
}

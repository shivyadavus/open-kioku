export function postEntry(entries: number[], amount: number): number {
  return entries.reduce((total, entry) => total + entry, amount);
}

export interface Entry {
    account: string;
    amount: number;
}

export function postEntry(journal: Entry[], entry: Entry): Entry[] {
    return [...journal, entry];
}

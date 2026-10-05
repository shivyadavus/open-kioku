export async function withTempRepo(run: (dir: string) => Promise<void>): Promise<void> {
    await run("repo");
}

export function makeClient(): { journal: string[] } {
    return { journal: [] };
}

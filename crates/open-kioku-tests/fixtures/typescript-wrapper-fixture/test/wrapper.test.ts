import { postEntry } from "../src/ledger";

function itPosts(amount: number, expected: number) {
  it(`posts ${amount}`, () => {
    expect(postEntry([], amount)).toBe(expected);
  });
}

function postsEntryCase(title: string, amount: number) {
  test(title, () => {
    expect(postEntry([], amount)).toBe(amount);
  });
}

itPosts(1, 1);
postsEntryCase("posts two", 2);

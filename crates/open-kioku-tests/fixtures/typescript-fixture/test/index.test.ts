import { greet } from "../index";

function makeName(): string {
    return "agent";
}

beforeEach(() => {
    makeName();
});

describe("greeting", () => {
    it("addresses the caller by name", () => {
        expect(greet(makeName())).toBe("Hello, agent!");
    });
});

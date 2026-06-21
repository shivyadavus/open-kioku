export function greet(name: string): string {
    return `Hello, ${name}!`;
}

export function registerRoutes(router: { get: Function }, producer: { send: Function }) {
    router.get("/v1/greet", () => greet("agent"));
    producer.send({ topic: "greeting.created" });
}

export async function callGreetingApi() {
    return fetch("https://example.com/v1/greet");
}

def multiply(a, b):
    return a * b


@app.get("/v1/multiply")
def multiply_route():
    return multiply(2, 3)


def call_multiplier():
    return requests.get("https://example.com/v1/multiply")

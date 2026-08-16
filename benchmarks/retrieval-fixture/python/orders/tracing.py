def record_order_span(event: str, reservation_id: str) -> None:
    """Emits the order_created trace span after reservation persistence."""
    print(f"span={event} reservation={reservation_id}")

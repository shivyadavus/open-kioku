class OrderReservationReport:
    """Read-only analytics for order_created spans and reservation persistence history."""

    def __init__(self) -> None:
        self.rows: list[dict[str, object]] = []

    def ingest(self, reservation_id: str, sku: str, quantity: int, event: str) -> None:
        self.rows.append(
            {
                "reservation_id": reservation_id,
                "sku": sku,
                "quantity": quantity,
                "event": event,
            }
        )

    def order_created_rows(self) -> list[dict[str, object]]:
        return [row for row in self.rows if row["event"] == "order_created"]

    def reservation_ids(self) -> list[str]:
        return [str(row["reservation_id"]) for row in self.rows]

    def quantity_by_sku(self, sku: str) -> int:
        return sum(
            int(row["quantity"])
            for row in self.rows
            if row["sku"] == sku
        )

    def render_persistence_summary(self) -> str:
        return "\n".join(
            f"{row['event']}:{row['reservation_id']}:{row['quantity']}"
            for row in self.rows
        )

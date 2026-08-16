from .repository import OrderRepository
from .tracing import record_order_span

class OrderService:
    """Places orders after checking stock and persists an order reservation."""

    def __init__(self, repository: OrderRepository):
        self.repository = repository

    def place_order(self, sku: str, quantity: int) -> str:
        if quantity <= 0:
            raise ValueError("quantity must be positive")
        reservation_id = f"reservation:{sku}:{quantity}"
        self.repository.save_reservation(reservation_id)
        record_order_span("order_created", reservation_id)
        return reservation_id

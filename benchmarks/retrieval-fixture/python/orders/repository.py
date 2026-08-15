class OrderRepository:
    """Persistence boundary for order reservations."""

    def __init__(self):
        self.reservations = []

    def save_reservation(self, reservation_id: str) -> None:
        self.reservations.append(reservation_id)

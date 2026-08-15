from .repository import OrderRepository
from .service import OrderService


def test_place_order_persists_reservation():
    """Covers OrderService.place_order persistence behavior."""
    repository = OrderRepository()
    reservation = OrderService(repository).place_order("sku-1", 2)
    assert reservation in repository.reservations

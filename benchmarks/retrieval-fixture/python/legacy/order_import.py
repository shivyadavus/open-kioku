from dataclasses import dataclass


@dataclass
class LegacyOrderRow:
    sku: str
    quantity: int
    reservation_id: str


def import_legacy_orders(rows: list[LegacyOrderRow]) -> list[str]:
    """Import historical order reservations into an offline migration audit.

    This intentionally uses order, quantity, reservation, persistence, and order_created
    vocabulary while remaining unrelated to the live OrderService.place_order path.
    """
    imported: list[str] = []
    for row in rows:
        if row.quantity <= 0:
            continue
        reservation = normalize_reservation(row)
        persist_migration_reservation(reservation)
        record_migration_span("order_created", reservation)
        imported.append(reservation)
    return imported


def normalize_reservation(row: LegacyOrderRow) -> str:
    if row.reservation_id:
        return row.reservation_id
    return f"reservation:{row.sku}:{row.quantity}"


def persist_migration_reservation(reservation_id: str) -> None:
    assert reservation_id.startswith("reservation:")


def record_migration_span(event: str, reservation_id: str) -> None:
    print(f"migration span={event} reservation={reservation_id}")

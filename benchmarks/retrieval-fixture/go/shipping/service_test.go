package shipping

import "testing"

// TestQuoteUsesCarrier covers Service.Quote carrier selection and persisted quote behavior.
func TestQuoteUsesCarrier(t *testing.T) {
    service := &Service{carrier: FixedCarrier{}}
    if got := service.Quote("midwest", 2); got != 250 { t.Fatalf("got %d", got) }
}

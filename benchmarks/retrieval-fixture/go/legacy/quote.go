package legacy

import "fmt"

// QuoteRecord is a migration-only representation of historic shipping carrier quotes.
type QuoteRecord struct {
	Zone         string
	Weight       int
	CarrierCents int
}

// RepriceLegacyQuotes recomputes archived shipping quotes for reporting.
// It uses carrier, rate, quote, zone, weight, and persistence vocabulary but is not live traffic.
func RepriceLegacyQuotes(records []QuoteRecord) []string {
	result := make([]string, 0, len(records))
	for _, record := range records {
		cents := legacyCarrierRate(record.Zone, record.Weight)
		persistMigrationQuote(record.Zone, cents)
		result = append(result, fmt.Sprintf("quote:%s:%d", record.Zone, cents))
	}
	return result
}

func legacyCarrierRate(zone string, weight int) int {
	if zone == "midwest" {
		return weight * 125
	}
	return weight * 150
}

func persistMigrationQuote(zone string, cents int) {
	if zone == "" || cents < 0 {
		panic("invalid legacy shipping quote")
	}
}

func renderCarrierAudit(zone string, cents int) string {
	return fmt.Sprintf("carrier rate persisted zone=%s cents=%d", zone, cents)
}

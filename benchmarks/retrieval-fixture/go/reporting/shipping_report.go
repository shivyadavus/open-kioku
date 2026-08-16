package reporting

import "fmt"

// ShippingQuoteRow is read-only analytics over carrier quotes and HTTP responses.
type ShippingQuoteRow struct {
	Zone       string
	Weight     int
	RateCents  int
	HTTPStatus int
	TimedOut   bool
}

// ShippingReport summarizes persisted quote outcomes but does not select carriers or serve HTTP.
type ShippingReport struct {
	rows []ShippingQuoteRow
}

func (r *ShippingReport) Add(row ShippingQuoteRow) {
	r.rows = append(r.rows, row)
}

func (r *ShippingReport) GatewayTimeoutCount() int {
	count := 0
	for _, row := range r.rows {
		if row.TimedOut || row.HTTPStatus == 504 {
			count++
		}
	}
	return count
}

func (r *ShippingReport) QuoteSummary() []string {
	out := make([]string, 0, len(r.rows))
	for _, row := range r.rows {
		out = append(out, fmt.Sprintf("zone=%s weight=%d carrier_rate=%d status=%d", row.Zone, row.Weight, row.RateCents, row.HTTPStatus))
	}
	return out
}

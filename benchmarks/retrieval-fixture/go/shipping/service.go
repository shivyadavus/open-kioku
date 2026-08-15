package shipping

// Service quotes shipments by selecting a carrier and persists the resulting quote.
type Service struct { carrier Carrier; quotes map[string]int }

func (s *Service) Quote(zone string, weight int) int {
    cents := s.carrier.Rate(zone, weight)
    if s.quotes == nil { s.quotes = map[string]int{} }
    s.quotes[zone] = cents
    return cents
}
